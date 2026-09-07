use std::fmt;

/// A declared field constraint that a value failed to satisfy.
///
/// Produced by [`super::FieldType::check_value`]. These are *value*
/// constraints written in the DSL — `text(max:)`, `integer(min:/max:)`,
/// `enum(...)`, `bytes(max:)` — as distinct from nullability, which is the
/// `required` modifier's job and is checked separately.
///
/// The variants carry the offending value alongside the bound so the HTTP
/// layer can render an actionable 422 without re-deriving anything.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConstraintViolation {
    /// A `text(max: N)` field received more than `max` characters.
    TextTooLong { len: usize, max: u32 },
    /// An `integer(min: N)` field received a smaller value.
    IntegerBelowMin { value: i64, min: i64 },
    /// An `integer(max: N)` field received a larger value.
    IntegerAboveMax { value: i64, max: i64 },
    /// An `enum(...)` field received a string that is not one of its variants.
    NotAnEnumVariant {
        value: String,
        allowed: Vec<String>,
    },
    /// A `bytes(max: N)` field received more than `max` bytes.
    BytesTooLarge { len: usize, max: usize },
}

impl fmt::Display for ConstraintViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TextTooLong { len, max } => write!(
                f,
                "text value of {len} characters exceeds the field's max length of {max}"
            ),
            Self::IntegerBelowMin { value, min } => {
                write!(f, "value {value} is below the field's minimum of {min}")
            }
            Self::IntegerAboveMax { value, max } => {
                write!(f, "value {value} is above the field's maximum of {max}")
            }
            Self::NotAnEnumVariant { value, allowed } => write!(
                f,
                "'{value}' is not one of the allowed values: {}",
                allowed.join(", ")
            ),
            Self::BytesTooLarge { len, max } => write!(
                f,
                "bytes value of {len} bytes exceeds the field's max_size of {max} bytes"
            ),
        }
    }
}

impl std::error::Error for ConstraintViolation {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_too_long_message_names_both_numbers() {
        let v = ConstraintViolation::TextTooLong { len: 40, max: 10 };
        assert_eq!(
            v.to_string(),
            "text value of 40 characters exceeds the field's max length of 10"
        );
    }

    #[test]
    fn integer_bounds_messages_are_directional() {
        assert_eq!(
            ConstraintViolation::IntegerBelowMin { value: 0, min: 1 }.to_string(),
            "value 0 is below the field's minimum of 1"
        );
        assert_eq!(
            ConstraintViolation::IntegerAboveMax { value: 9, max: 5 }.to_string(),
            "value 9 is above the field's maximum of 5"
        );
    }

    #[test]
    fn enum_message_lists_the_allowed_variants() {
        let v = ConstraintViolation::NotAnEnumVariant {
            value: "gamma".into(),
            allowed: vec!["alpha".into(), "beta".into()],
        };
        assert_eq!(
            v.to_string(),
            "'gamma' is not one of the allowed values: alpha, beta"
        );
    }

    #[test]
    fn bytes_message_matches_the_legacy_wording() {
        // The `bytes` cap was the one constraint already enforced in-process.
        // Keep its message byte-identical so existing clients see no change.
        let v = ConstraintViolation::BytesTooLarge { len: 128, max: 64 };
        assert_eq!(
            v.to_string(),
            "bytes value of 128 bytes exceeds the field's max_size of 64 bytes"
        );
    }
}
