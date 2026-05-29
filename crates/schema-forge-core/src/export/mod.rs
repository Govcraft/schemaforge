//! Export serialization core.
//!
//! Pure, IO-free value mapping shared by the export endpoints, the async export
//! job actor, and the bundle writers (items 5-8 of the export epic). See
//! [`serialize`] for the [`DynamicValue`](crate::types::DynamicValue) →
//! cell/NDJSON flatten policy.
//!
//! ## File fields
//!
//! [`DynamicValue`](crate::types::DynamicValue) has no dedicated `File` variant:
//! a `file` field is persisted as a
//! [`FileAttachment`](crate::types::FileAttachment) inside its JSONB column and
//! therefore surfaces here as a [`Composite`](crate::types::DynamicValue::Composite)
//! or [`Json`](crate::types::DynamicValue::Json) value carrying that metadata
//! (key, size, mime, checksum, status, timestamps). The serialization core emits
//! that metadata as-is — URL/metadata only — and **never** materializes a raw
//! blob. Pulling raw file bytes into a ZIP bundle is the bundle writer's job
//! (item 8), not this pure core's, which keeps "file → URL/metadata only" true
//! by construction here.

pub mod serialize;

pub use serialize::{
    csv_record, to_cell, to_ndjson, to_xlsx_cell, BytesPolicy, CellOptions, NoDisplay,
    RelationDisplay, XlsxCell,
};
