use std::fmt;
use std::str::FromStr;

use mti::prelude::{MagicTypeId, MagicTypeIdExt, V7};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::types::{
    Cardinality, DefaultValue, DynamicValue, FieldDefinition, FieldModifier, FieldName, FieldType,
    SchemaId, SchemaName,
};

// ---------------------------------------------------------------------------
// MigrationId
// ---------------------------------------------------------------------------

const MIGRATION_PREFIX: &str = "migration";

/// A TypeID-based identifier with prefix "migration".
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MigrationId(MagicTypeId);

impl MigrationId {
    /// Generates a new random `MigrationId` using UUIDv7.
    pub fn new() -> Self {
        Self(MIGRATION_PREFIX.create_type_id::<V7>())
    }

    /// Parses a `MigrationId` from its string representation, validating the "migration" prefix.
    pub fn parse(s: &str) -> Result<Self, MigrationError> {
        let id = MagicTypeId::from_str(s)
            .map_err(|e| MigrationError::InvalidMigrationId(format!("{e}")))?;
        if id.prefix().as_str() != MIGRATION_PREFIX {
            return Err(MigrationError::InvalidMigrationId(format!(
                "expected prefix '{MIGRATION_PREFIX}', got '{}'",
                id.prefix().as_str()
            )));
        }
        Ok(Self(id))
    }

    /// Returns the string representation of this id.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl Default for MigrationId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for MigrationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for MigrationId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.as_str())
    }
}

impl<'de> Deserialize<'de> for MigrationId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// MigrationSafety
// ---------------------------------------------------------------------------

/// Classification of how safe a migration step is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MigrationSafety {
    /// The step is safe and can be applied automatically.
    Safe,
    /// The step may require confirmation (e.g. adding a required field without default).
    RequiresConfirmation,
    /// The step is destructive and may cause data loss (e.g. dropping a field or schema).
    Destructive,
}

impl fmt::Display for MigrationSafety {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Safe => write!(f, "safe"),
            Self::RequiresConfirmation => write!(f, "requires_confirmation"),
            Self::Destructive => write!(f, "destructive"),
        }
    }
}

// ---------------------------------------------------------------------------
// ValueTransform
// ---------------------------------------------------------------------------

/// Describes how to convert existing field values when a field type changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transform")]
#[non_exhaustive]
pub enum ValueTransform {
    /// No conversion needed (compatible types).
    Identity,
    /// Convert integer to float.
    IntegerToFloat,
    /// Convert float to integer (truncation).
    FloatToInteger,
    /// Convert any scalar to its text representation.
    ToString,
    /// Set all existing values to a specific default.
    SetDefault { value: DefaultValue },
    /// Set all existing values to null.
    SetNull,
}

impl fmt::Display for ValueTransform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Identity => write!(f, "identity"),
            Self::IntegerToFloat => write!(f, "integer_to_float"),
            Self::FloatToInteger => write!(f, "float_to_integer"),
            Self::ToString => write!(f, "to_string"),
            Self::SetDefault { value } => write!(f, "set_default({value})"),
            Self::SetNull => write!(f, "set_null"),
        }
    }
}

// ---------------------------------------------------------------------------
// MigrationStep
// ---------------------------------------------------------------------------

/// A single, atomic migration operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step")]
#[non_exhaustive]
pub enum MigrationStep {
    /// Create a new schema table.
    CreateSchema {
        name: SchemaName,
        fields: Vec<FieldDefinition>,
    },
    /// Drop an entire schema table.
    DropSchema { name: SchemaName },
    /// Add a new field to an existing schema.
    AddField { field: FieldDefinition },
    /// Remove a field from an existing schema.
    RemoveField { name: FieldName },
    /// Rename a field.
    RenameField {
        old_name: FieldName,
        new_name: FieldName,
    },
    /// Change a field's type, with an optional value transform.
    ChangeType {
        name: FieldName,
        old_type: FieldType,
        new_type: FieldType,
        transform: ValueTransform,
    },
    /// Add an index on a field.
    AddIndex { field: FieldName },
    /// Remove an index from a field.
    RemoveIndex { field: FieldName },
    /// Add a relation field.
    AddRelation {
        name: FieldName,
        target: SchemaName,
        cardinality: Cardinality,
    },
    /// Remove a relation field.
    RemoveRelation { name: FieldName },
    /// Backfill a newly required field with a default value.
    BackfillRequired {
        field: FieldName,
        default_value: DynamicValue,
    },
    /// Add a required modifier to an existing field.
    AddRequired { field: FieldName },
    /// Remove a required modifier from an existing field.
    RemoveRequired { field: FieldName },
    /// Set or change a default value for a field.
    SetDefault {
        field: FieldName,
        value: DefaultValue,
    },
    /// Remove a default value from a field.
    RemoveDefault { field: FieldName },
}

impl MigrationStep {
    /// Classify the safety level of this migration step.
    pub fn safety(&self) -> MigrationSafety {
        match self {
            Self::CreateSchema { .. }
            | Self::AddField { .. }
            | Self::AddIndex { .. }
            | Self::AddRelation { .. }
            | Self::RemoveIndex { .. }
            | Self::RemoveRequired { .. }
            | Self::SetDefault { .. }
            | Self::RemoveDefault { .. } => MigrationSafety::Safe,

            Self::RenameField { .. }
            | Self::ChangeType { .. }
            | Self::BackfillRequired { .. }
            | Self::AddRequired { .. } => MigrationSafety::RequiresConfirmation,

            Self::DropSchema { .. } | Self::RemoveField { .. } | Self::RemoveRelation { .. } => {
                MigrationSafety::Destructive
            }
        }
    }
}

impl fmt::Display for MigrationStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateSchema { name, fields } => {
                write!(f, "CREATE schema '{name}' with {} fields", fields.len())
            }
            Self::DropSchema { name } => write!(f, "DROP schema '{name}'"),
            Self::AddField { field } => {
                write!(f, "ADD field '{}'", field.name)
            }
            Self::RemoveField { name } => write!(f, "REMOVE field '{name}'"),
            Self::RenameField { old_name, new_name } => {
                write!(f, "RENAME field '{old_name}' to '{new_name}'")
            }
            Self::ChangeType {
                name,
                old_type,
                new_type,
                transform,
            } => {
                write!(
                    f,
                    "CHANGE TYPE of '{name}' from {old_type} to {new_type} via {transform}"
                )
            }
            Self::AddIndex { field } => write!(f, "ADD INDEX on '{field}'"),
            Self::RemoveIndex { field } => write!(f, "REMOVE INDEX on '{field}'"),
            Self::AddRelation {
                name,
                target,
                cardinality,
            } => {
                write!(f, "ADD RELATION '{name}' -> {target} ({cardinality})")
            }
            Self::RemoveRelation { name } => write!(f, "REMOVE RELATION '{name}'"),
            Self::BackfillRequired {
                field,
                default_value,
            } => {
                write!(f, "BACKFILL '{field}' with {default_value}")
            }
            Self::AddRequired { field } => write!(f, "ADD REQUIRED on '{field}'"),
            Self::RemoveRequired { field } => write!(f, "REMOVE REQUIRED on '{field}'"),
            Self::SetDefault { field, value } => {
                write!(f, "SET DEFAULT on '{field}' to {value}")
            }
            Self::RemoveDefault { field } => write!(f, "REMOVE DEFAULT on '{field}'"),
        }
    }
}

// ---------------------------------------------------------------------------
// MigrationPlan
// ---------------------------------------------------------------------------

/// A complete migration plan: an ordered list of steps with metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MigrationPlan {
    /// Unique identifier for this migration.
    pub id: MigrationId,
    /// The schema this migration applies to.
    pub schema_id: SchemaId,
    /// The target schema name.
    pub schema_name: SchemaName,
    /// Ordered list of migration steps.
    pub steps: Vec<MigrationStep>,
}

impl MigrationPlan {
    /// Creates a new migration plan.
    pub fn new(schema_id: SchemaId, schema_name: SchemaName, steps: Vec<MigrationStep>) -> Self {
        Self {
            id: MigrationId::new(),
            schema_id,
            schema_name,
            steps,
        }
    }

    /// Returns the overall safety classification of the entire plan.
    /// The plan is as safe as its least safe step.
    pub fn overall_safety(&self) -> MigrationSafety {
        let mut worst = MigrationSafety::Safe;
        for step in &self.steps {
            let s = step.safety();
            worst = match (worst, s) {
                (MigrationSafety::Destructive, _) | (_, MigrationSafety::Destructive) => {
                    MigrationSafety::Destructive
                }
                (MigrationSafety::RequiresConfirmation, _)
                | (_, MigrationSafety::RequiresConfirmation) => {
                    MigrationSafety::RequiresConfirmation
                }
                _ => MigrationSafety::Safe,
            };
        }
        worst
    }

    /// Returns true if the plan has no steps.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Returns the number of steps.
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Returns true if all steps are safe.
    pub fn is_safe(&self) -> bool {
        self.overall_safety() == MigrationSafety::Safe
    }

    /// Returns true if any step is destructive.
    pub fn has_destructive_steps(&self) -> bool {
        self.steps
            .iter()
            .any(|s| s.safety() == MigrationSafety::Destructive)
    }
}

impl fmt::Display for MigrationPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "Migration plan for '{}' ({} steps, {})",
            self.schema_name,
            self.steps.len(),
            self.overall_safety()
        )?;
        for (i, step) in self.steps.iter().enumerate() {
            writeln!(f, "  {}. {} [{}]", i + 1, step, step.safety())?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DiffEngine
// ---------------------------------------------------------------------------

/// Pure function module for computing schema diffs.
pub struct DiffEngine;

impl DiffEngine {
    /// Compare two schema definitions and produce a migration plan.
    ///
    /// This is a pure function: no I/O, no side effects.
    pub fn diff(
        old: &crate::types::SchemaDefinition,
        new: &crate::types::SchemaDefinition,
    ) -> MigrationPlan {
        Self::diff_with_renames(old, new, &[])
    }

    /// Compare two schema definitions with explicit rename hints.
    ///
    /// Rename hints are `(old_name, new_name)` pairs. When a rename hint is
    /// provided and the old field exists, a `RenameField` step is emitted
    /// instead of `RemoveField` + `AddField`. If the type also changed,
    /// a `ChangeType` step is emitted for the new name.
    pub fn diff_with_renames(
        old: &crate::types::SchemaDefinition,
        new: &crate::types::SchemaDefinition,
        renames: &[(FieldName, FieldName)],
    ) -> MigrationPlan {
        let mut steps = Vec::new();

        Self::diff_fields_with_renames(old, new, renames, &mut steps);
        Self::diff_modifiers_with_renames(old, new, renames, &mut steps);

        MigrationPlan::new(new.id.clone(), new.name.clone(), steps)
    }

    /// Create a migration plan for a brand new schema (no old version).
    pub fn create_new(schema: &crate::types::SchemaDefinition) -> MigrationPlan {
        let steps = vec![MigrationStep::CreateSchema {
            name: schema.name.clone(),
            fields: schema.fields.clone(),
        }];
        MigrationPlan::new(schema.id.clone(), schema.name.clone(), steps)
    }

    fn diff_fields_with_renames(
        old: &crate::types::SchemaDefinition,
        new: &crate::types::SchemaDefinition,
        renames: &[(FieldName, FieldName)],
        steps: &mut Vec<MigrationStep>,
    ) {
        use std::collections::HashMap;

        // Build rename maps: old_name → new_name, new_name → old_name
        let rename_old_to_new: HashMap<&str, &FieldName> = renames
            .iter()
            .map(|(old_n, new_n)| (old_n.as_str(), new_n))
            .collect();
        let rename_new_to_old: HashMap<&str, &FieldName> = renames
            .iter()
            .map(|(old_n, new_n)| (new_n.as_str(), old_n))
            .collect();

        // Emit RenameField for valid rename pairs
        for (old_name, new_name) in renames {
            if let Some(old_field) = old.field(old_name.as_str()) {
                steps.push(MigrationStep::RenameField {
                    old_name: old_name.clone(),
                    new_name: new_name.clone(),
                });

                // If the type also changed, emit ChangeType using the new name
                if let Some(new_field) = new.field(new_name.as_str()) {
                    if old_field.field_type != new_field.field_type {
                        Self::emit_change_type_with_name(
                            new_name.clone(),
                            old_field,
                            new_field,
                            steps,
                        );
                    }
                }
            }
        }

        // Detect removed fields — skip rename sources
        for old_field in &old.fields {
            if rename_old_to_new.contains_key(old_field.name.as_str()) {
                continue;
            }
            if new.field(old_field.name.as_str()).is_none() {
                Self::emit_remove_field(old_field, steps);
            }
        }

        // Detect added fields — skip rename targets
        for new_field in &new.fields {
            if rename_new_to_old.contains_key(new_field.name.as_str()) {
                continue;
            }
            if old.field(new_field.name.as_str()).is_none() {
                Self::emit_add_field(new_field, steps);
            }
        }

        // Detect changed fields (same name, different type) — skip renamed fields
        for new_field in &new.fields {
            if rename_new_to_old.contains_key(new_field.name.as_str()) {
                continue; // already handled above
            }
            if let Some(old_field) = old.field(new_field.name.as_str()) {
                if old_field.field_type != new_field.field_type {
                    Self::emit_change_type(old_field, new_field, steps);
                }
            }
        }
    }

    fn diff_modifiers_with_renames(
        old: &crate::types::SchemaDefinition,
        new: &crate::types::SchemaDefinition,
        renames: &[(FieldName, FieldName)],
        steps: &mut Vec<MigrationStep>,
    ) {
        use std::collections::HashMap;

        let rename_new_to_old: HashMap<&str, &FieldName> = renames
            .iter()
            .map(|(old_n, new_n)| (new_n.as_str(), old_n))
            .collect();

        for new_field in &new.fields {
            // Find the corresponding old field: either by same name, or via rename
            let old_field = if let Some(old_name) = rename_new_to_old.get(new_field.name.as_str()) {
                old.field(old_name.as_str())
            } else {
                old.field(new_field.name.as_str())
            };

            if let Some(old_field) = old_field {
                // For renamed fields, we compare old type with new type
                // (type changes are handled in diff_fields_with_renames)
                // Modifier diffing uses the new field's name for step output
                let old_type = &old_field.field_type;
                let new_type = &new_field.field_type;
                if old_type == new_type {
                    Self::diff_field_modifiers(old_field, new_field, steps);
                }
            }
        }
    }

    fn diff_field_modifiers(
        old_field: &FieldDefinition,
        new_field: &FieldDefinition,
        steps: &mut Vec<MigrationStep>,
    ) {
        let old_required = old_field.is_required();
        let new_required = new_field.is_required();
        let old_indexed = old_field.is_indexed();
        let new_indexed = new_field.is_indexed();
        let old_default = Self::extract_default(&old_field.modifiers);
        let new_default = Self::extract_default(&new_field.modifiers);

        // Required changes
        if !old_required && new_required {
            steps.push(MigrationStep::AddRequired {
                field: new_field.name.clone(),
            });
        } else if old_required && !new_required {
            steps.push(MigrationStep::RemoveRequired {
                field: new_field.name.clone(),
            });
        }

        // Index changes
        if !old_indexed && new_indexed {
            steps.push(MigrationStep::AddIndex {
                field: new_field.name.clone(),
            });
        } else if old_indexed && !new_indexed {
            steps.push(MigrationStep::RemoveIndex {
                field: new_field.name.clone(),
            });
        }

        // Default value changes
        match (old_default, new_default) {
            (None, Some(val)) => {
                steps.push(MigrationStep::SetDefault {
                    field: new_field.name.clone(),
                    value: val.clone(),
                });
            }
            (Some(_), None) => {
                steps.push(MigrationStep::RemoveDefault {
                    field: new_field.name.clone(),
                });
            }
            (Some(old_val), Some(new_val)) if old_val != new_val => {
                steps.push(MigrationStep::SetDefault {
                    field: new_field.name.clone(),
                    value: new_val.clone(),
                });
            }
            _ => {}
        }
    }

    fn extract_default(modifiers: &[FieldModifier]) -> Option<&DefaultValue> {
        modifiers.iter().find_map(|m| {
            if let FieldModifier::Default { value } = m {
                Some(value)
            } else {
                None
            }
        })
    }

    fn emit_remove_field(field: &FieldDefinition, steps: &mut Vec<MigrationStep>) {
        if matches!(field.field_type, FieldType::Relation { .. }) {
            steps.push(MigrationStep::RemoveRelation {
                name: field.name.clone(),
            });
        } else {
            steps.push(MigrationStep::RemoveField {
                name: field.name.clone(),
            });
        }
    }

    fn emit_add_field(field: &FieldDefinition, steps: &mut Vec<MigrationStep>) {
        if let FieldType::Relation {
            target,
            cardinality,
        } = &field.field_type
        {
            steps.push(MigrationStep::AddRelation {
                name: field.name.clone(),
                target: target.clone(),
                cardinality: *cardinality,
            });
        } else {
            steps.push(MigrationStep::AddField {
                field: field.clone(),
            });
        }
    }

    fn emit_change_type(
        old_field: &FieldDefinition,
        new_field: &FieldDefinition,
        steps: &mut Vec<MigrationStep>,
    ) {
        Self::emit_change_type_with_name(
            new_field.name.clone(),
            old_field,
            new_field,
            steps,
        );
    }

    fn emit_change_type_with_name(
        name: FieldName,
        old_field: &FieldDefinition,
        new_field: &FieldDefinition,
        steps: &mut Vec<MigrationStep>,
    ) {
        let transform = Self::infer_transform(&old_field.field_type, &new_field.field_type);
        steps.push(MigrationStep::ChangeType {
            name,
            old_type: old_field.field_type.clone(),
            new_type: new_field.field_type.clone(),
            transform,
        });
    }

    fn infer_transform(old: &FieldType, new: &FieldType) -> ValueTransform {
        match (old, new) {
            (FieldType::Integer(_), FieldType::Float(_)) => ValueTransform::IntegerToFloat,
            (FieldType::Float(_), FieldType::Integer(_)) => ValueTransform::FloatToInteger,
            (_, FieldType::Text(_)) => ValueTransform::ToString,
            _ => ValueTransform::SetNull,
        }
    }
}

// ---------------------------------------------------------------------------
// MigrationError
// ---------------------------------------------------------------------------

/// Errors that occur during migration planning or execution.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MigrationError {
    /// The migration ID string could not be parsed.
    InvalidMigrationId(String),
    /// Attempted to apply a destructive migration without confirmation.
    DestructiveWithoutConfirmation { step_description: String },
    /// A required field was added without a default value for backfill.
    RequiredFieldWithoutDefault { field_name: String },
    /// Type conversion is not supported between the given types.
    UnsupportedTypeConversion {
        field_name: String,
        from_type: String,
        to_type: String,
    },
    /// The migration plan is empty (no steps to apply).
    EmptyMigrationPlan,
}

impl fmt::Display for MigrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMigrationId(s) => {
                write!(f, "invalid migration id: {s}")
            }
            Self::DestructiveWithoutConfirmation { step_description } => {
                write!(
                    f,
                    "destructive migration step requires confirmation: {step_description}"
                )
            }
            Self::RequiredFieldWithoutDefault { field_name } => {
                write!(
                    f,
                    "required field '{field_name}' was added without a default value for backfill"
                )
            }
            Self::UnsupportedTypeConversion {
                field_name,
                from_type,
                to_type,
            } => {
                write!(
                    f,
                    "unsupported type conversion for field '{field_name}': {from_type} -> {to_type}"
                )
            }
            Self::EmptyMigrationPlan => {
                write!(f, "migration plan has no steps to apply")
            }
        }
    }
}

impl std::error::Error for MigrationError {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        EnumVariants, FloatConstraints, IntegerConstraints, SchemaDefinition, TextConstraints,
    };

    // -- MigrationId tests --

    #[test]
    fn migration_id_has_correct_prefix() {
        let id = MigrationId::new();
        assert!(
            id.as_str().starts_with("migration_"),
            "expected 'migration_' prefix, got: {}",
            id
        );
    }

    #[test]
    fn migration_id_parse_roundtrip() {
        let id = MigrationId::new();
        let parsed = MigrationId::parse(id.as_str()).unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn migration_id_parse_wrong_prefix() {
        let wrong = "entity_01h455vb4pex5vsknk084sn02q";
        assert!(MigrationId::parse(wrong).is_err());
    }

    #[test]
    fn migration_id_display_matches_as_str() {
        let id = MigrationId::new();
        assert_eq!(id.to_string(), id.as_str());
    }

    #[test]
    fn migration_id_serde_roundtrip() {
        let id = MigrationId::new();
        let json = serde_json::to_string(&id).unwrap();
        let back: MigrationId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    // -- MigrationSafety tests --

    #[test]
    fn safety_display() {
        assert_eq!(MigrationSafety::Safe.to_string(), "safe");
        assert_eq!(
            MigrationSafety::RequiresConfirmation.to_string(),
            "requires_confirmation"
        );
        assert_eq!(MigrationSafety::Destructive.to_string(), "destructive");
    }

    #[test]
    fn safety_serde_roundtrip() {
        for s in [
            MigrationSafety::Safe,
            MigrationSafety::RequiresConfirmation,
            MigrationSafety::Destructive,
        ] {
            let json = serde_json::to_string(&s).unwrap();
            let back: MigrationSafety = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }

    // -- ValueTransform tests --

    #[test]
    fn value_transform_display() {
        assert_eq!(ValueTransform::Identity.to_string(), "identity");
        assert_eq!(
            ValueTransform::IntegerToFloat.to_string(),
            "integer_to_float"
        );
        assert_eq!(
            ValueTransform::FloatToInteger.to_string(),
            "float_to_integer"
        );
        assert_eq!(ValueTransform::ToString.to_string(), "to_string");
        assert_eq!(ValueTransform::SetNull.to_string(), "set_null");
        assert_eq!(
            ValueTransform::SetDefault {
                value: DefaultValue::Integer(0)
            }
            .to_string(),
            "set_default(0)"
        );
    }

    #[test]
    fn value_transform_serde_roundtrip() {
        let transforms = vec![
            ValueTransform::Identity,
            ValueTransform::IntegerToFloat,
            ValueTransform::FloatToInteger,
            ValueTransform::ToString,
            ValueTransform::SetNull,
            ValueTransform::SetDefault {
                value: DefaultValue::Boolean(true),
            },
        ];
        for t in transforms {
            let json = serde_json::to_string(&t).unwrap();
            let back: ValueTransform = serde_json::from_str(&json).unwrap();
            assert_eq!(t, back);
        }
    }

    // -- MigrationStep tests --

    #[test]
    fn step_safety_classification() {
        let safe_steps = vec![
            MigrationStep::CreateSchema {
                name: SchemaName::new("Test").unwrap(),
                fields: vec![make_field("name")],
            },
            MigrationStep::AddField {
                field: make_field("email"),
            },
            MigrationStep::AddIndex {
                field: FieldName::new("email").unwrap(),
            },
            MigrationStep::RemoveIndex {
                field: FieldName::new("email").unwrap(),
            },
            MigrationStep::RemoveRequired {
                field: FieldName::new("email").unwrap(),
            },
            MigrationStep::SetDefault {
                field: FieldName::new("status").unwrap(),
                value: DefaultValue::String("active".into()),
            },
            MigrationStep::RemoveDefault {
                field: FieldName::new("status").unwrap(),
            },
        ];

        for step in &safe_steps {
            assert_eq!(
                step.safety(),
                MigrationSafety::Safe,
                "Expected safe: {step}"
            );
        }

        let confirm_steps = vec![
            MigrationStep::RenameField {
                old_name: FieldName::new("name").unwrap(),
                new_name: FieldName::new("full_name").unwrap(),
            },
            MigrationStep::ChangeType {
                name: FieldName::new("score").unwrap(),
                old_type: FieldType::Integer(IntegerConstraints::unconstrained()),
                new_type: FieldType::Float(FloatConstraints::unconstrained()),
                transform: ValueTransform::IntegerToFloat,
            },
            MigrationStep::AddRequired {
                field: FieldName::new("email").unwrap(),
            },
        ];

        for step in &confirm_steps {
            assert_eq!(
                step.safety(),
                MigrationSafety::RequiresConfirmation,
                "Expected requires_confirmation: {step}"
            );
        }

        let destructive_steps = vec![
            MigrationStep::DropSchema {
                name: SchemaName::new("Old").unwrap(),
            },
            MigrationStep::RemoveField {
                name: FieldName::new("old_field").unwrap(),
            },
            MigrationStep::RemoveRelation {
                name: FieldName::new("company").unwrap(),
            },
        ];

        for step in &destructive_steps {
            assert_eq!(
                step.safety(),
                MigrationSafety::Destructive,
                "Expected destructive: {step}"
            );
        }
    }

    #[test]
    fn step_display() {
        let step = MigrationStep::AddField {
            field: make_field("email"),
        };
        assert_eq!(step.to_string(), "ADD field 'email'");

        let step = MigrationStep::RemoveField {
            name: FieldName::new("old_field").unwrap(),
        };
        assert_eq!(step.to_string(), "REMOVE field 'old_field'");

        let step = MigrationStep::RenameField {
            old_name: FieldName::new("name").unwrap(),
            new_name: FieldName::new("full_name").unwrap(),
        };
        assert_eq!(step.to_string(), "RENAME field 'name' to 'full_name'");
    }

    #[test]
    fn step_serde_roundtrip() {
        let steps = vec![
            MigrationStep::CreateSchema {
                name: SchemaName::new("Contact").unwrap(),
                fields: vec![make_field("name")],
            },
            MigrationStep::AddField {
                field: make_field("email"),
            },
            MigrationStep::RemoveField {
                name: FieldName::new("old_field").unwrap(),
            },
            MigrationStep::AddIndex {
                field: FieldName::new("email").unwrap(),
            },
        ];
        for step in steps {
            let json = serde_json::to_string(&step).unwrap();
            let back: MigrationStep = serde_json::from_str(&json).unwrap();
            assert_eq!(step, back);
        }
    }

    // -- MigrationPlan tests --

    #[test]
    fn plan_overall_safety_empty() {
        let plan = MigrationPlan::new(SchemaId::new(), SchemaName::new("Test").unwrap(), vec![]);
        assert!(plan.is_empty());
        assert_eq!(plan.overall_safety(), MigrationSafety::Safe);
        assert!(plan.is_safe());
        assert!(!plan.has_destructive_steps());
    }

    #[test]
    fn plan_overall_safety_safe() {
        let plan = MigrationPlan::new(
            SchemaId::new(),
            SchemaName::new("Test").unwrap(),
            vec![
                MigrationStep::AddField {
                    field: make_field("email"),
                },
                MigrationStep::AddIndex {
                    field: FieldName::new("email").unwrap(),
                },
            ],
        );
        assert_eq!(plan.len(), 2);
        assert!(plan.is_safe());
    }

    #[test]
    fn plan_overall_safety_destructive() {
        let plan = MigrationPlan::new(
            SchemaId::new(),
            SchemaName::new("Test").unwrap(),
            vec![
                MigrationStep::AddField {
                    field: make_field("email"),
                },
                MigrationStep::RemoveField {
                    name: FieldName::new("old_field").unwrap(),
                },
            ],
        );
        assert_eq!(plan.overall_safety(), MigrationSafety::Destructive);
        assert!(plan.has_destructive_steps());
        assert!(!plan.is_safe());
    }

    #[test]
    fn plan_display() {
        let plan = MigrationPlan::new(
            SchemaId::new(),
            SchemaName::new("Contact").unwrap(),
            vec![MigrationStep::AddField {
                field: make_field("phone"),
            }],
        );
        let display = plan.to_string();
        assert!(display.contains("Migration plan for 'Contact'"));
        assert!(display.contains("1 steps"));
        assert!(display.contains("ADD field 'phone'"));
    }

    #[test]
    fn plan_serde_roundtrip() {
        let plan = MigrationPlan::new(
            SchemaId::new(),
            SchemaName::new("Contact").unwrap(),
            vec![
                MigrationStep::AddField {
                    field: make_field("phone"),
                },
                MigrationStep::AddIndex {
                    field: FieldName::new("phone").unwrap(),
                },
            ],
        );
        let json = serde_json::to_string(&plan).unwrap();
        let back: MigrationPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, back);
    }

    // -- DiffEngine tests --

    #[test]
    fn diff_identical_schemas_produces_empty_plan() {
        let schema = make_schema("Contact", vec![make_field("name"), make_field("email")]);
        let plan = DiffEngine::diff(&schema, &schema);
        assert!(plan.is_empty());
    }

    #[test]
    fn diff_detects_added_field() {
        let old = make_schema("Contact", vec![make_field("name")]);
        let new = make_schema("Contact", vec![make_field("name"), make_field("email")]);
        let plan = DiffEngine::diff(&old, &new);
        assert_eq!(plan.len(), 1);
        assert!(
            matches!(&plan.steps[0], MigrationStep::AddField { field } if field.name.as_str() == "email")
        );
    }

    #[test]
    fn diff_detects_removed_field() {
        let old = make_schema("Contact", vec![make_field("name"), make_field("email")]);
        let new = make_schema("Contact", vec![make_field("name")]);
        let plan = DiffEngine::diff(&old, &new);
        assert_eq!(plan.len(), 1);
        assert!(
            matches!(&plan.steps[0], MigrationStep::RemoveField { name } if name.as_str() == "email")
        );
    }

    #[test]
    fn diff_detects_type_change() {
        let old = make_schema(
            "Stats",
            vec![FieldDefinition::new(
                FieldName::new("score").unwrap(),
                FieldType::Integer(IntegerConstraints::unconstrained()),
            )],
        );
        let new = make_schema(
            "Stats",
            vec![FieldDefinition::new(
                FieldName::new("score").unwrap(),
                FieldType::Float(FloatConstraints::unconstrained()),
            )],
        );
        let plan = DiffEngine::diff(&old, &new);
        assert_eq!(plan.len(), 1);
        assert!(matches!(
            &plan.steps[0],
            MigrationStep::ChangeType { name, transform: ValueTransform::IntegerToFloat, .. }
            if name.as_str() == "score"
        ));
    }

    #[test]
    fn diff_detects_modifier_changes() {
        let old = make_schema(
            "Contact",
            vec![FieldDefinition::new(
                FieldName::new("email").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            )],
        );
        let new = make_schema(
            "Contact",
            vec![FieldDefinition::with_modifiers(
                FieldName::new("email").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Required, FieldModifier::Indexed],
            )],
        );
        let plan = DiffEngine::diff(&old, &new);
        assert_eq!(plan.len(), 2);
        assert!(plan.steps.iter().any(
            |s| matches!(s, MigrationStep::AddRequired { field } if field.as_str() == "email")
        ));
        assert!(plan
            .steps
            .iter()
            .any(|s| matches!(s, MigrationStep::AddIndex { field } if field.as_str() == "email")));
    }

    #[test]
    fn diff_detects_added_relation() {
        let old = make_schema("Contact", vec![make_field("name")]);
        let new = make_schema(
            "Contact",
            vec![
                make_field("name"),
                FieldDefinition::new(
                    FieldName::new("company").unwrap(),
                    FieldType::Relation {
                        target: SchemaName::new("Company").unwrap(),
                        cardinality: Cardinality::One,
                    },
                ),
            ],
        );
        let plan = DiffEngine::diff(&old, &new);
        assert_eq!(plan.len(), 1);
        assert!(matches!(
            &plan.steps[0],
            MigrationStep::AddRelation { name, target, cardinality: Cardinality::One }
            if name.as_str() == "company" && target.as_str() == "Company"
        ));
    }

    #[test]
    fn diff_detects_removed_relation() {
        let old = make_schema(
            "Contact",
            vec![
                make_field("name"),
                FieldDefinition::new(
                    FieldName::new("company").unwrap(),
                    FieldType::Relation {
                        target: SchemaName::new("Company").unwrap(),
                        cardinality: Cardinality::One,
                    },
                ),
            ],
        );
        let new = make_schema("Contact", vec![make_field("name")]);
        let plan = DiffEngine::diff(&old, &new);
        assert_eq!(plan.len(), 1);
        assert!(matches!(
            &plan.steps[0],
            MigrationStep::RemoveRelation { name } if name.as_str() == "company"
        ));
    }

    #[test]
    fn diff_detects_default_value_changes() {
        let old = make_schema(
            "Contact",
            vec![FieldDefinition::with_modifiers(
                FieldName::new("status").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Default {
                    value: DefaultValue::String("active".into()),
                }],
            )],
        );
        let new = make_schema(
            "Contact",
            vec![FieldDefinition::with_modifiers(
                FieldName::new("status").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Default {
                    value: DefaultValue::String("pending".into()),
                }],
            )],
        );
        let plan = DiffEngine::diff(&old, &new);
        assert_eq!(plan.len(), 1);
        assert!(matches!(
            &plan.steps[0],
            MigrationStep::SetDefault { field, value: DefaultValue::String(s) }
            if field.as_str() == "status" && s == "pending"
        ));
    }

    #[test]
    fn diff_detects_default_value_removed() {
        let old = make_schema(
            "Contact",
            vec![FieldDefinition::with_modifiers(
                FieldName::new("status").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Default {
                    value: DefaultValue::String("active".into()),
                }],
            )],
        );
        let new = make_schema(
            "Contact",
            vec![FieldDefinition::new(
                FieldName::new("status").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            )],
        );
        let plan = DiffEngine::diff(&old, &new);
        assert_eq!(plan.len(), 1);
        assert!(matches!(
            &plan.steps[0],
            MigrationStep::RemoveDefault { field } if field.as_str() == "status"
        ));
    }

    #[test]
    fn create_new_produces_single_create_step() {
        let schema = make_schema("Contact", vec![make_field("name"), make_field("email")]);
        let plan = DiffEngine::create_new(&schema);
        assert_eq!(plan.len(), 1);
        assert!(matches!(
            &plan.steps[0],
            MigrationStep::CreateSchema { name, fields }
            if name.as_str() == "Contact" && fields.len() == 2
        ));
        assert!(plan.is_safe());
    }

    #[test]
    fn diff_complex_schema_evolution() {
        // Simulate evolving a CRM Contact schema:
        // Old: name (text), email (text, required)
        // New: name (text), email (text, required, indexed), phone (text), status (enum)
        let old = make_schema(
            "Contact",
            vec![
                make_field("name"),
                FieldDefinition::with_modifiers(
                    FieldName::new("email").unwrap(),
                    FieldType::Text(TextConstraints::with_max_length(255)),
                    vec![FieldModifier::Required],
                ),
            ],
        );
        let new = make_schema(
            "Contact",
            vec![
                make_field("name"),
                FieldDefinition::with_modifiers(
                    FieldName::new("email").unwrap(),
                    FieldType::Text(TextConstraints::with_max_length(255)),
                    vec![FieldModifier::Required, FieldModifier::Indexed],
                ),
                make_field("phone"),
                FieldDefinition::new(
                    FieldName::new("status").unwrap(),
                    FieldType::Enum(
                        EnumVariants::new(vec!["Active".into(), "Inactive".into()]).unwrap(),
                    ),
                ),
            ],
        );
        let plan = DiffEngine::diff(&old, &new);
        // Should detect: add phone, add status, add index on email
        assert_eq!(plan.len(), 3);
        assert!(plan.steps.iter().any(
            |s| matches!(s, MigrationStep::AddField { field } if field.name.as_str() == "phone")
        ));
        assert!(plan.steps.iter().any(
            |s| matches!(s, MigrationStep::AddField { field } if field.name.as_str() == "status")
        ));
        assert!(plan
            .steps
            .iter()
            .any(|s| matches!(s, MigrationStep::AddIndex { field } if field.as_str() == "email")));
    }

    // -- DiffEngine rename tests --

    #[test]
    fn diff_with_renames_detects_rename() {
        let old = make_schema("Contact", vec![make_field("name"), make_field("email")]);
        let new = make_schema("Contact", vec![make_field("full_name"), make_field("email")]);
        let renames = vec![(
            FieldName::new("name").unwrap(),
            FieldName::new("full_name").unwrap(),
        )];
        let plan = DiffEngine::diff_with_renames(&old, &new, &renames);
        assert_eq!(plan.len(), 1);
        assert!(matches!(
            &plan.steps[0],
            MigrationStep::RenameField { old_name, new_name }
            if old_name.as_str() == "name" && new_name.as_str() == "full_name"
        ));
        // Should NOT contain RemoveField or AddField
        assert!(!plan.steps.iter().any(|s| matches!(s, MigrationStep::RemoveField { .. })));
        assert!(!plan.steps.iter().any(|s| matches!(s, MigrationStep::AddField { .. })));
    }

    #[test]
    fn diff_with_renames_rename_with_type_change() {
        let old = make_schema(
            "Stats",
            vec![FieldDefinition::new(
                FieldName::new("score").unwrap(),
                FieldType::Integer(IntegerConstraints::unconstrained()),
            )],
        );
        let new = make_schema(
            "Stats",
            vec![FieldDefinition::new(
                FieldName::new("rating").unwrap(),
                FieldType::Float(FloatConstraints::unconstrained()),
            )],
        );
        let renames = vec![(
            FieldName::new("score").unwrap(),
            FieldName::new("rating").unwrap(),
        )];
        let plan = DiffEngine::diff_with_renames(&old, &new, &renames);
        assert_eq!(plan.len(), 2);
        assert!(plan.steps.iter().any(|s| matches!(
            s,
            MigrationStep::RenameField { old_name, new_name }
            if old_name.as_str() == "score" && new_name.as_str() == "rating"
        )));
        assert!(plan.steps.iter().any(|s| matches!(
            s,
            MigrationStep::ChangeType { name, transform: ValueTransform::IntegerToFloat, .. }
            if name.as_str() == "rating"
        )));
    }

    #[test]
    fn diff_with_renames_no_hint_still_deletes() {
        let old = make_schema("Contact", vec![make_field("name"), make_field("email")]);
        let new = make_schema("Contact", vec![make_field("full_name"), make_field("email")]);
        // No rename hints — should produce RemoveField + AddField
        let plan = DiffEngine::diff_with_renames(&old, &new, &[]);
        assert!(plan.steps.iter().any(|s| matches!(
            s, MigrationStep::RemoveField { name } if name.as_str() == "name"
        )));
        assert!(plan.steps.iter().any(|s| matches!(
            s, MigrationStep::AddField { field } if field.name.as_str() == "full_name"
        )));
        assert!(!plan.steps.iter().any(|s| matches!(s, MigrationStep::RenameField { .. })));
    }

    // -- MigrationError tests --

    #[test]
    fn migration_error_display() {
        let cases = vec![
            (
                MigrationError::InvalidMigrationId("bad".into()),
                "invalid migration id: bad",
            ),
            (
                MigrationError::DestructiveWithoutConfirmation {
                    step_description: "DROP schema 'Contact'".into(),
                },
                "destructive migration step requires confirmation: DROP schema 'Contact'",
            ),
            (
                MigrationError::RequiredFieldWithoutDefault {
                    field_name: "email".into(),
                },
                "required field 'email' was added without a default value for backfill",
            ),
            (
                MigrationError::UnsupportedTypeConversion {
                    field_name: "score".into(),
                    from_type: "Boolean".into(),
                    to_type: "Integer".into(),
                },
                "unsupported type conversion for field 'score': Boolean -> Integer",
            ),
            (
                MigrationError::EmptyMigrationPlan,
                "migration plan has no steps to apply",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn migration_error_is_std_error() {
        let err: Box<dyn std::error::Error> = Box::new(MigrationError::EmptyMigrationPlan);
        assert!(err.to_string().contains("no steps"));
    }

    // -- Test helpers --

    fn make_field(name: &str) -> FieldDefinition {
        FieldDefinition::new(
            FieldName::new(name).unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )
    }

    fn make_schema(name: &str, fields: Vec<FieldDefinition>) -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new(name).unwrap(),
            fields,
            vec![],
        )
        .unwrap()
    }
}
