//! Pure serialization core for the export capability.
//!
//! Maps a [`DynamicValue`] to a CSV/XLSX cell string and to an NDJSON
//! [`serde_json::Value`] under the single documented flatten policy from
//! ADR-0003. Every function here is **pure**: no IO, no async, no clock, no
//! storage. The async job, streaming, storage, and authz layers (items 5-8)
//! consume these functions; they never live here.
//!
//! # Flatten policy (ADR-0003)
//!
//! | `DynamicValue` shape | CSV / XLSX cell | NDJSON value |
//! | --- | --- | --- |
//! | scalar (text/int/float/bool/datetime/duration/enum) | value as-is | value as-is |
//! | relation (one) | resolved display value | `{ id, display }` object |
//! | relation (many) / array / map / composite | JSON-encoded in the cell | native, lossless |
//! | bytes | omitted by default; base64 only on opt-in | base64 string |
//! | file | URL / metadata only | URL / metadata only |
//!
//! CSV is the rectangular projection (lossy where it must be); NDJSON is the
//! lossless dump. XLSX shares the CSV cell rendering (it is also rectangular).
//!
//! The `flatten: json` field hint ([`ExportFlatten::Json`]) forces JSON-in-cell
//! rendering for the rectangular formats even for values that would otherwise
//! render plainly. It never affects the NDJSON path, which is always lossless.

use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::types::{base64, format_go_duration, DynamicValue, EntityId, ExportFlatten};

/// Resolves an [`EntityId`] to its `@display` string for a relation field.
///
/// This is the single injected dependency that keeps the serialization core
/// pure: consumers (items 5-8) batch-resolve display values from the target
/// schema's `@display(...)` field and hand the lookup in here. A relation whose
/// target row is missing, unreadable, or has no display value resolves to
/// `None`, which renders as an empty cell (CSV/XLSX) or a `null` display
/// (NDJSON).
pub trait RelationDisplay {
    /// Returns the resolved display string for `id`, or `None` when it cannot
    /// be resolved.
    fn display_for(&self, id: &EntityId) -> Option<String>;
}

/// A [`RelationDisplay`] that resolves nothing.
///
/// Useful for tests and for callers that have not resolved displays: relations
/// fall back to the raw id string in the rectangular formats and to
/// `{ id, display: null }` in NDJSON.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoDisplay;

impl RelationDisplay for NoDisplay {
    fn display_for(&self, _id: &EntityId) -> Option<String> {
        None
    }
}

/// Blanket impl so any `Fn(&EntityId) -> Option<String>` can be used directly.
impl<F> RelationDisplay for F
where
    F: Fn(&EntityId) -> Option<String>,
{
    fn display_for(&self, id: &EntityId) -> Option<String> {
        (self)(id)
    }
}

/// Whether opaque binary (`bytes`) values may leave in a rectangular file.
///
/// Fail-closed: bytes are [`omitted`](BytesPolicy::Omit) from CSV/XLSX by
/// default and only base64-encoded into a cell when the caller explicitly opts
/// in via [`BytesPolicy::Base64`]. NDJSON always base64-encodes bytes (it is the
/// lossless dump); this policy does not affect NDJSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BytesPolicy {
    /// Render `bytes` as an empty cell in CSV/XLSX (the fail-closed default).
    #[default]
    Omit,
    /// Render `bytes` as a base64 string in CSV/XLSX (explicit opt-in).
    Base64,
}

/// Per-cell rendering options for the rectangular (CSV/XLSX) path.
#[derive(Debug, Clone, Copy, Default)]
pub struct CellOptions {
    /// Field-level `@exportable(flatten: ...)` hint, if any. [`ExportFlatten::Json`]
    /// forces JSON-in-cell rendering for scalars that would otherwise render plainly.
    pub flatten: Option<ExportFlatten>,
    /// How to treat `bytes` values in the rectangular formats.
    pub bytes: BytesPolicy,
}

impl CellOptions {
    /// Convenience constructor for the common "no hint, omit bytes" case.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the flatten hint.
    pub fn with_flatten(mut self, flatten: Option<ExportFlatten>) -> Self {
        self.flatten = flatten;
        self
    }

    /// Sets the bytes policy.
    pub fn with_bytes(mut self, bytes: BytesPolicy) -> Self {
        self.bytes = bytes;
        self
    }
}

/// Renders a [`DynamicValue`] to a CSV/XLSX cell string under the flatten policy.
///
/// This is the rectangular projection and is lossy where it must be: structured
/// values (relation-many / array / map / composite) are JSON-encoded into the
/// cell, relation-one becomes its resolved display string, bytes are omitted by
/// default, and a file renders as its URL/metadata. The returned `String` is the
/// raw cell content; quoting/escaping is the `csv` writer's job, not ours.
pub fn to_cell<D: RelationDisplay + ?Sized>(
    value: &DynamicValue,
    opts: &CellOptions,
    displays: &D,
) -> String {
    // A flatten:json hint forces JSON-in-cell for everything except null, which
    // stays an empty cell so empty and the JSON literal `null` are not conflated.
    if matches!(opts.flatten, Some(ExportFlatten::Json)) && !matches!(value, DynamicValue::Null) {
        return json_in_cell(&to_ndjson(value, displays));
    }

    match value {
        DynamicValue::Null => String::new(),
        DynamicValue::Text(s) => s.clone(),
        DynamicValue::Integer(i) => i.to_string(),
        DynamicValue::Float(v) => v.to_string(),
        DynamicValue::Boolean(b) => b.to_string(),
        DynamicValue::DateTime(dt) => dt.to_rfc3339(),
        DynamicValue::Duration(d) => format_go_duration(d),
        DynamicValue::Enum(s) => s.clone(),
        DynamicValue::Bytes(b) => match opts.bytes {
            BytesPolicy::Omit => String::new(),
            BytesPolicy::Base64 => base64::encode_standard(b),
        },
        // relation(one): resolved display value, falling back to the raw id.
        DynamicValue::Ref(id) => displays.display_for(id).unwrap_or_else(|| id.to_string()),
        // relation(many) / array / map / composite / json: JSON-encoded in the cell.
        DynamicValue::RefArray(_)
        | DynamicValue::Array(_)
        | DynamicValue::Map(_)
        | DynamicValue::Composite(_)
        | DynamicValue::Json(_) => json_in_cell(&to_ndjson(value, displays)),
    }
}

/// Renders a [`DynamicValue`] to a lossless NDJSON [`JsonValue`].
///
/// This is the lossless dump: scalars map to their natural JSON form, a
/// relation-one becomes `{ "id", "display" }`, relation-many becomes an array of
/// such objects, arrays/maps/composites map recursively, bytes become a base64
/// string, and JSON passes through verbatim.
pub fn to_ndjson<D: RelationDisplay + ?Sized>(value: &DynamicValue, displays: &D) -> JsonValue {
    match value {
        DynamicValue::Null => JsonValue::Null,
        DynamicValue::Text(s) => JsonValue::String(s.clone()),
        DynamicValue::Integer(i) => JsonValue::Number((*i).into()),
        DynamicValue::Float(v) => serde_json::Number::from_f64(*v)
            .map(JsonValue::Number)
            // Non-finite floats (NaN/inf) have no JSON number; emit null rather
            // than panic, keeping the dump well-formed.
            .unwrap_or(JsonValue::Null),
        DynamicValue::Boolean(b) => JsonValue::Bool(*b),
        DynamicValue::DateTime(dt) => JsonValue::String(dt.to_rfc3339()),
        DynamicValue::Duration(d) => JsonValue::String(format_go_duration(d)),
        DynamicValue::Enum(s) => JsonValue::String(s.clone()),
        DynamicValue::Bytes(b) => JsonValue::String(base64::encode_standard(b)),
        DynamicValue::Json(v) => v.clone(),
        DynamicValue::Ref(id) => relation_object(id, displays),
        DynamicValue::RefArray(ids) => {
            JsonValue::Array(ids.iter().map(|id| relation_object(id, displays)).collect())
        }
        DynamicValue::Array(arr) => {
            JsonValue::Array(arr.iter().map(|v| to_ndjson(v, displays)).collect())
        }
        DynamicValue::Map(map) | DynamicValue::Composite(map) => {
            let mut out = JsonMap::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), to_ndjson(v, displays));
            }
            JsonValue::Object(out)
        }
    }
}

/// Builds the `{ "id", "display" }` object used for relations in NDJSON.
fn relation_object<D: RelationDisplay + ?Sized>(id: &EntityId, displays: &D) -> JsonValue {
    let mut obj = JsonMap::with_capacity(2);
    obj.insert("id".to_string(), JsonValue::String(id.to_string()));
    obj.insert(
        "display".to_string(),
        displays
            .display_for(id)
            .map_or(JsonValue::Null, JsonValue::String),
    );
    JsonValue::Object(obj)
}

/// Serializes one rectangular row of already-rendered cells into a single CSV
/// record (including the trailing newline), using the `csv` crate's quoting and
/// escaping rules.
///
/// This is a pure helper over the cell strings produced by [`to_cell`]: it owns
/// the RFC 4180 quoting (commas, quotes, and newlines inside a cell), so the
/// streaming and buffered writers (items 6-8) never re-implement escaping. An
/// empty `cells` slice yields a single empty (quoted) field followed by the
/// terminator, since the `csv` writer cannot emit a zero-field record.
///
/// # Errors
///
/// Returns the underlying [`csv::Error`] only if the `csv` writer fails to
/// serialize the record. For an in-memory string buffer this does not occur in
/// practice, but the result is surfaced rather than unwrapped so callers stay in
/// control of error handling.
pub fn csv_record<S: AsRef<[u8]>>(cells: &[S]) -> Result<String, csv::Error> {
    let mut wtr = csv::WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .from_writer(Vec::new());
    wtr.write_record(cells.iter().map(AsRef::as_ref))?;
    wtr.flush()?;
    let bytes = wtr
        .into_inner()
        .map_err(|e| csv::Error::from(e.into_error()))?;
    // The csv writer only emits the bytes we fed it plus its own ASCII delimiters
    // and terminators, so the buffer is always valid UTF-8.
    Ok(String::from_utf8(bytes).unwrap_or_default())
}

/// Encodes a JSON value as the compact string that lands in a rectangular cell.
///
/// A JSON string scalar is unwrapped to its bare text so a `text` value rendered
/// through the JSON path is not double-quoted; every other shape is serialized
/// compactly. Serialization of an in-memory `JsonValue` cannot fail, so the
/// fallback is unreachable in practice.
fn json_in_cell(value: &JsonValue) -> String {
    match value {
        JsonValue::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn id(prefix: &str) -> EntityId {
        // Deterministic-enough for assertions that only compare to the same id.
        EntityId::new(prefix)
    }

    // ---- scalars: CSV ----

    #[test]
    fn cell_null_is_empty() {
        assert_eq!(to_cell(&DynamicValue::Null, &CellOptions::new(), &NoDisplay), "");
    }

    #[test]
    fn cell_text_as_is() {
        let v = DynamicValue::Text("hello, world".into());
        assert_eq!(to_cell(&v, &CellOptions::new(), &NoDisplay), "hello, world");
    }

    #[test]
    fn cell_integer() {
        assert_eq!(
            to_cell(&DynamicValue::Integer(-42), &CellOptions::new(), &NoDisplay),
            "-42"
        );
    }

    #[test]
    fn cell_float() {
        assert_eq!(
            to_cell(&DynamicValue::Float(2.5), &CellOptions::new(), &NoDisplay),
            "2.5"
        );
    }

    #[test]
    fn cell_boolean() {
        assert_eq!(
            to_cell(&DynamicValue::Boolean(true), &CellOptions::new(), &NoDisplay),
            "true"
        );
    }

    #[test]
    fn cell_datetime_is_rfc3339() {
        let dt = chrono::DateTime::parse_from_rfc3339("2026-05-29T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let v = DynamicValue::DateTime(dt);
        assert_eq!(
            to_cell(&v, &CellOptions::new(), &NoDisplay),
            "2026-05-29T12:00:00+00:00"
        );
    }

    #[test]
    fn cell_duration_is_go_format() {
        let v = DynamicValue::Duration(chrono::TimeDelta::seconds(90));
        assert_eq!(to_cell(&v, &CellOptions::new(), &NoDisplay), "90s");
    }

    #[test]
    fn cell_enum_as_is() {
        let v = DynamicValue::Enum("Active".into());
        assert_eq!(to_cell(&v, &CellOptions::new(), &NoDisplay), "Active");
    }

    // ---- bytes: fail-closed ----

    #[test]
    fn cell_bytes_omitted_by_default() {
        let v = DynamicValue::Bytes(b"hello".to_vec());
        assert_eq!(to_cell(&v, &CellOptions::new(), &NoDisplay), "");
    }

    #[test]
    fn cell_bytes_base64_on_opt_in() {
        let v = DynamicValue::Bytes(b"hello".to_vec());
        let opts = CellOptions::new().with_bytes(BytesPolicy::Base64);
        assert_eq!(to_cell(&v, &opts, &NoDisplay), "aGVsbG8=");
    }

    #[test]
    fn ndjson_bytes_always_base64() {
        let v = DynamicValue::Bytes(b"hello".to_vec());
        assert_eq!(to_ndjson(&v, &NoDisplay), JsonValue::String("aGVsbG8=".into()));
    }

    // ---- relation one ----

    #[test]
    fn cell_relation_one_uses_resolved_display() {
        let rid = id("user");
        let target = rid.clone();
        let resolver = move |q: &EntityId| {
            (q == &target).then(|| "Ada Lovelace".to_string())
        };
        let v = DynamicValue::Ref(rid);
        assert_eq!(to_cell(&v, &CellOptions::new(), &resolver), "Ada Lovelace");
    }

    #[test]
    fn cell_relation_one_falls_back_to_id() {
        let rid = id("user");
        let v = DynamicValue::Ref(rid.clone());
        assert_eq!(to_cell(&v, &CellOptions::new(), &NoDisplay), rid.to_string());
    }

    #[test]
    fn ndjson_relation_one_is_id_display_object() {
        let rid = id("user");
        let target = rid.clone();
        let resolver = move |q: &EntityId| (q == &target).then(|| "Ada".to_string());
        let v = DynamicValue::Ref(rid.clone());
        let out = to_ndjson(&v, &resolver);
        assert_eq!(out["id"], JsonValue::String(rid.to_string()));
        assert_eq!(out["display"], JsonValue::String("Ada".into()));
    }

    #[test]
    fn ndjson_relation_one_unresolved_display_is_null() {
        let rid = id("user");
        let v = DynamicValue::Ref(rid.clone());
        let out = to_ndjson(&v, &NoDisplay);
        assert_eq!(out["id"], JsonValue::String(rid.to_string()));
        assert_eq!(out["display"], JsonValue::Null);
    }

    // ---- relation many ----

    #[test]
    fn cell_relation_many_is_json_array_of_objects() {
        let a = id("user");
        let b = id("user");
        let v = DynamicValue::RefArray(vec![a.clone(), b.clone()]);
        let cell = to_cell(&v, &CellOptions::new(), &NoDisplay);
        let parsed: JsonValue = serde_json::from_str(&cell).unwrap();
        assert!(parsed.is_array());
        assert_eq!(parsed[0]["id"], JsonValue::String(a.to_string()));
        assert_eq!(parsed[1]["display"], JsonValue::Null);
    }

    #[test]
    fn ndjson_relation_many_is_lossless_array() {
        let a = id("user");
        let v = DynamicValue::RefArray(vec![a.clone()]);
        let out = to_ndjson(&v, &NoDisplay);
        assert_eq!(out[0]["id"], JsonValue::String(a.to_string()));
    }

    // ---- array ----

    #[test]
    fn cell_array_is_json() {
        let v = DynamicValue::Array(vec![
            DynamicValue::Integer(1),
            DynamicValue::Text("x".into()),
        ]);
        assert_eq!(to_cell(&v, &CellOptions::new(), &NoDisplay), "[1,\"x\"]");
    }

    #[test]
    fn ndjson_array_is_native() {
        let v = DynamicValue::Array(vec![DynamicValue::Integer(1), DynamicValue::Boolean(false)]);
        let out = to_ndjson(&v, &NoDisplay);
        assert_eq!(out, serde_json::json!([1, false]));
    }

    // ---- map ----

    #[test]
    fn cell_map_is_json_object() {
        let mut m = BTreeMap::new();
        m.insert("k".to_string(), DynamicValue::Text("v".into()));
        let v = DynamicValue::Map(m);
        assert_eq!(to_cell(&v, &CellOptions::new(), &NoDisplay), "{\"k\":\"v\"}");
    }

    #[test]
    fn ndjson_map_is_native_object() {
        let mut m = BTreeMap::new();
        m.insert("a".to_string(), DynamicValue::Integer(1));
        let v = DynamicValue::Map(m);
        assert_eq!(to_ndjson(&v, &NoDisplay), serde_json::json!({ "a": 1 }));
    }

    // ---- composite ----

    #[test]
    fn cell_composite_is_json_object() {
        let mut m = BTreeMap::new();
        m.insert("name".to_string(), DynamicValue::Text("n".into()));
        m.insert("count".to_string(), DynamicValue::Integer(3));
        let v = DynamicValue::Composite(m);
        let cell = to_cell(&v, &CellOptions::new(), &NoDisplay);
        let parsed: JsonValue = serde_json::from_str(&cell).unwrap();
        assert_eq!(parsed["name"], JsonValue::String("n".into()));
        assert_eq!(parsed["count"], serde_json::json!(3));
    }

    #[test]
    fn ndjson_composite_nested_is_lossless() {
        let mut inner = BTreeMap::new();
        inner.insert("x".to_string(), DynamicValue::Float(1.5));
        let mut outer = BTreeMap::new();
        outer.insert("inner".to_string(), DynamicValue::Composite(inner));
        let v = DynamicValue::Composite(outer);
        assert_eq!(
            to_ndjson(&v, &NoDisplay),
            serde_json::json!({ "inner": { "x": 1.5 } })
        );
    }

    // ---- json passthrough ----

    #[test]
    fn cell_json_object_is_compact() {
        let v = DynamicValue::Json(serde_json::json!({ "a": [1, 2] }));
        assert_eq!(to_cell(&v, &CellOptions::new(), &NoDisplay), "{\"a\":[1,2]}");
    }

    #[test]
    fn cell_json_string_scalar_is_unquoted() {
        let v = DynamicValue::Json(JsonValue::String("plain".into()));
        assert_eq!(to_cell(&v, &CellOptions::new(), &NoDisplay), "plain");
    }

    #[test]
    fn ndjson_json_passthrough() {
        let payload = serde_json::json!({ "nested": { "k": [true, null] } });
        let v = DynamicValue::Json(payload.clone());
        assert_eq!(to_ndjson(&v, &NoDisplay), payload);
    }

    // ---- flatten:json hint ----

    #[test]
    fn flatten_json_forces_scalar_text_to_json_string() {
        // A plain text scalar under flatten:json renders as a JSON string literal.
        let v = DynamicValue::Text("hi".into());
        let opts = CellOptions::new().with_flatten(Some(ExportFlatten::Json));
        // json_in_cell unwraps the JSON string, so the bare text is preserved.
        assert_eq!(to_cell(&v, &opts, &NoDisplay), "hi");
    }

    #[test]
    fn flatten_json_forces_integer_to_json_number_text() {
        let v = DynamicValue::Integer(7);
        let opts = CellOptions::new().with_flatten(Some(ExportFlatten::Json));
        assert_eq!(to_cell(&v, &opts, &NoDisplay), "7");
    }

    #[test]
    fn flatten_json_null_stays_empty_cell() {
        let opts = CellOptions::new().with_flatten(Some(ExportFlatten::Json));
        assert_eq!(to_cell(&DynamicValue::Null, &opts, &NoDisplay), "");
    }

    #[test]
    fn flatten_json_array_matches_default_json_in_cell() {
        let v = DynamicValue::Array(vec![DynamicValue::Integer(1)]);
        let opts = CellOptions::new().with_flatten(Some(ExportFlatten::Json));
        assert_eq!(to_cell(&v, &opts, &NoDisplay), "[1]");
    }

    #[test]
    fn flatten_json_does_not_affect_ndjson() {
        // to_ndjson has no flatten parameter — it is always lossless. This test
        // documents that the lossless path is independent of the cell hint.
        let v = DynamicValue::Integer(7);
        assert_eq!(to_ndjson(&v, &NoDisplay), serde_json::json!(7));
    }

    // ---- non-finite float guard ----

    #[test]
    fn ndjson_non_finite_float_is_null() {
        let v = DynamicValue::Float(f64::NAN);
        assert_eq!(to_ndjson(&v, &NoDisplay), JsonValue::Null);
    }

    // ---- csv_record ----

    #[test]
    fn csv_record_simple_row() {
        let row = ["a", "b", "c"];
        assert_eq!(csv_record(&row).unwrap(), "a,b,c\r\n");
    }

    #[test]
    fn csv_record_quotes_cells_with_commas_quotes_and_newlines() {
        let row = ["plain", "has,comma", "has\"quote", "has\nnewline"];
        assert_eq!(
            csv_record(&row).unwrap(),
            "plain,\"has,comma\",\"has\"\"quote\",\"has\nnewline\"\r\n"
        );
    }

    #[test]
    fn csv_record_empty_row_is_single_empty_quoted_field() {
        // The csv writer cannot emit a zero-field record; an empty `cells` slice
        // yields one empty (quoted) field followed by the terminator.
        let row: [&str; 0] = [];
        assert_eq!(csv_record(&row).unwrap(), "\"\"\r\n");
    }

    #[test]
    fn csv_record_round_trips_rendered_cells() {
        let cells = [
            to_cell(&DynamicValue::Text("hello, world".into()), &CellOptions::new(), &NoDisplay),
            to_cell(&DynamicValue::Integer(42), &CellOptions::new(), &NoDisplay),
        ];
        let record = csv_record(&cells).unwrap();
        let mut rdr = csv::ReaderBuilder::new()
            .has_headers(false)
            .from_reader(record.as_bytes());
        let parsed = rdr.records().next().unwrap().unwrap();
        assert_eq!(&parsed[0], "hello, world");
        assert_eq!(&parsed[1], "42");
    }

    // ---- NoDisplay helper ----

    #[test]
    fn no_display_resolves_nothing() {
        assert_eq!(NoDisplay.display_for(&id("x")), None);
    }
}
