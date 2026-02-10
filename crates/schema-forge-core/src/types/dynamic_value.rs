use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::entity_id::EntityId;

/// Runtime value for any field type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
#[non_exhaustive]
pub enum DynamicValue {
    Null,
    Text(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    DateTime(chrono::DateTime<chrono::Utc>),
    Enum(String),
    Json(serde_json::Value),
    Array(Vec<DynamicValue>),
    Composite(BTreeMap<String, DynamicValue>),
    Ref(EntityId),
    RefArray(Vec<EntityId>),
}

impl std::fmt::Display for DynamicValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Null => write!(f, "null"),
            Self::Text(s) => write!(f, "\"{s}\""),
            Self::Integer(i) => write!(f, "{i}"),
            Self::Float(v) => write!(f, "{v}"),
            Self::Boolean(b) => write!(f, "{b}"),
            Self::DateTime(dt) => write!(f, "{dt}"),
            Self::Enum(s) => write!(f, "{s}"),
            Self::Json(v) => write!(f, "{v}"),
            Self::Array(arr) => {
                write!(f, "[")?;
                for (i, v) in arr.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
            Self::Composite(map) => {
                write!(f, "{{")?;
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
            Self::Ref(id) => write!(f, "ref({id})"),
            Self::RefArray(ids) => {
                write!(f, "refs[")?;
                for (i, id) in ids.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{id}")?;
                }
                write!(f, "]")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_null() {
        assert_eq!(DynamicValue::Null.to_string(), "null");
    }

    #[test]
    fn display_text() {
        assert_eq!(DynamicValue::Text("hi".into()).to_string(), "\"hi\"");
    }

    #[test]
    fn display_integer() {
        assert_eq!(DynamicValue::Integer(42).to_string(), "42");
    }

    #[test]
    fn display_boolean() {
        assert_eq!(DynamicValue::Boolean(true).to_string(), "true");
    }

    #[test]
    fn serde_roundtrip_primitives() {
        let values = vec![
            DynamicValue::Null,
            DynamicValue::Text("hello".into()),
            DynamicValue::Integer(42),
            DynamicValue::Float(2.72),
            DynamicValue::Boolean(false),
            DynamicValue::Enum("Active".into()),
        ];
        for v in values {
            let json = serde_json::to_string(&v).unwrap();
            let back: DynamicValue = serde_json::from_str(&json).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn serde_roundtrip_array() {
        let v = DynamicValue::Array(vec![
            DynamicValue::Integer(1),
            DynamicValue::Integer(2),
        ]);
        let json = serde_json::to_string(&v).unwrap();
        let back: DynamicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn serde_roundtrip_composite() {
        let mut map = BTreeMap::new();
        map.insert("key".to_string(), DynamicValue::Text("val".into()));
        let v = DynamicValue::Composite(map);
        let json = serde_json::to_string(&v).unwrap();
        let back: DynamicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn serde_roundtrip_json() {
        let v = DynamicValue::Json(serde_json::json!({"key": [1, 2, 3]}));
        let json = serde_json::to_string(&v).unwrap();
        let back: DynamicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn serde_roundtrip_ref() {
        let v = DynamicValue::Ref(EntityId::new());
        let json = serde_json::to_string(&v).unwrap();
        let back: DynamicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn serde_roundtrip_ref_array() {
        let v = DynamicValue::RefArray(vec![EntityId::new(), EntityId::new()]);
        let json = serde_json::to_string(&v).unwrap();
        let back: DynamicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn serde_roundtrip_datetime() {
        let dt = chrono::Utc::now();
        let v = DynamicValue::DateTime(dt);
        let json = serde_json::to_string(&v).unwrap();
        let back: DynamicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }
}
