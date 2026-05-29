//! The CEL value model.
//!
//! This is the engine's own value type. It bridges to SchemaForge's
//! `DynamicValue` (the `bridge` submodule lands with the evaluator core, #108)
//! and additionally carries CEL-internal types the DSL does not yet expose —
//! `uint`, `bytes`, `duration` (tracked by field-type issues #98/#97/#96).
//!
//! Note: equality is currently `derive`d. The evaluator core (#108) replaces it
//! with CEL cross-type numeric equality (`1 == 1u == 1.0`, `NaN != NaN`).

use std::collections::BTreeMap;

use chrono::{DateTime, TimeDelta, Utc};

/// A CEL runtime value.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum CelValue {
    /// The null value.
    Null,
    /// A boolean.
    Bool(bool),
    /// A signed 64-bit integer (`int`).
    Int(i64),
    /// An unsigned 64-bit integer (`uint`).
    Uint(u64),
    /// A 64-bit float (`double`).
    Double(f64),
    /// A UTF-8 string.
    String(String),
    /// A byte string (`bytes`).
    Bytes(Vec<u8>),
    /// A point in time (`google.protobuf.Timestamp`).
    Timestamp(DateTime<Utc>),
    /// A signed duration (`google.protobuf.Duration`).
    Duration(TimeDelta),
    /// A list of values.
    List(Vec<CelValue>),
    /// A map keyed by `bool`/`int`/`uint`/`string`.
    Map(BTreeMap<CelKey, CelValue>),
    /// A type value (the result of `type(x)` and of type identifiers).
    Type(String),
}

impl CelValue {
    /// The CEL runtime type of this value.
    pub fn cel_type(&self) -> CelType {
        match self {
            Self::Null => CelType::Null,
            Self::Bool(_) => CelType::Bool,
            Self::Int(_) => CelType::Int,
            Self::Uint(_) => CelType::Uint,
            Self::Double(_) => CelType::Double,
            Self::String(_) => CelType::String,
            Self::Bytes(_) => CelType::Bytes,
            Self::Timestamp(_) => CelType::Timestamp,
            Self::Duration(_) => CelType::Duration,
            Self::List(_) => CelType::List,
            Self::Map(_) => CelType::Map,
            Self::Type(_) => CelType::Type,
        }
    }
}

/// A legal CEL map key. Per `cel.expr.Value`, only `bool`, `int`, `uint`, and
/// `string` may key a map.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum CelKey {
    /// A boolean key.
    Bool(bool),
    /// A signed integer key.
    Int(i64),
    /// An unsigned integer key.
    Uint(u64),
    /// A string key.
    String(String),
}

/// The CEL type lattice (the kinds this engine represents).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CelType {
    /// `null_type`
    Null,
    /// `bool`
    Bool,
    /// `int`
    Int,
    /// `uint`
    Uint,
    /// `double`
    Double,
    /// `string`
    String,
    /// `bytes`
    Bytes,
    /// `google.protobuf.Timestamp`
    Timestamp,
    /// `google.protobuf.Duration`
    Duration,
    /// `list`
    List,
    /// `map`
    Map,
    /// `type`
    Type,
}
