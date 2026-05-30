//! Pure ZIP-bundle assembly for the async export path (ADR-0003, item 8).
//!
//! A ZIP export carries more than one member in a single archive: the
//! rectangular/lossless data file (CSV or NDJSON) plus, when the entity's
//! `@export(bundle_files: true)` opt-in is set, the raw blobs of its `file`
//! fields. Bundling file blobs is a strictly larger exfiltration surface than the
//! URL-/metadata-only treatment the flat formats give a `file` field, so it is
//! reachable **only** behind that explicit opt-in and the same `Export` authz +
//! AU-2 audit gates the rest of the export path enforces.
//!
//! Everything in this module is a pure function over already-read,
//! already-field-stripped inputs: the archive layout, the member-name policy, and
//! the deterministic ZIP writer are all unit-testable without any storage,
//! network, or actor. The job actor (which owns the supervised future) does the
//! impure work — pulling each blob from the [`ExportArtifactStore`] — and feeds
//! the bytes into [`build_zip_bundle`].

use schema_forge_backend::entity::Entity;
use schema_forge_core::types::{
    DynamicValue, FieldType, FileAttachment, FileStatus, SchemaDefinition,
};

/// One member of the archive: a path inside the ZIP plus its bytes.
///
/// `name` is a forward-slash archive path (e.g. `data.csv` or
/// `files/<field>/<entity_id>/<filename>`); `bytes` is the member's full content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleEntry {
    /// Archive-internal path, always forward-slash separated.
    pub name: String,
    /// Full member content.
    pub bytes: Vec<u8>,
}

impl BundleEntry {
    /// Construct an entry from an archive path and its bytes.
    pub fn new(name: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            bytes,
        }
    }
}

/// A pointer to a single `file`-field blob that should be pulled from storage and
/// embedded in the bundle.
///
/// Produced purely from already-stripped entities by [`collect_file_blob_refs`]:
/// it carries the storage `key` to fetch and the archive `member_name` to land it
/// under. The actual fetch (the impure step) is the caller's job, keeping this
/// module storage-free and testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileBlobRef {
    /// Object-storage key of the blob (from the `FileAttachment`).
    pub key: String,
    /// Deterministic archive path the blob lands under in the bundle.
    pub member_name: String,
}

/// The archive directory under which `file`-field blobs are placed.
pub const BUNDLE_FILES_DIR: &str = "files";

/// Build the archive member name for one file blob.
///
/// Layout: `files/<field>/<entity_id>/<filename>`. The `entity_id` segment keeps
/// blobs from different rows that share a filename from colliding; the `field`
/// segment groups a row's multiple file fields. `filename` falls back to the last
/// path segment of the storage key when the attachment carries no friendlier
/// name, and any path separators in it are flattened so a crafted name cannot
/// escape the `files/` subtree (zip-slip defense).
pub fn blob_member_name(field: &str, entity_id: &str, filename: &str) -> String {
    let safe = sanitize_archive_segment(filename);
    format!("{BUNDLE_FILES_DIR}/{field}/{entity_id}/{safe}")
}

/// Flatten an untrusted name into a single safe archive path segment.
///
/// Strips directory components and rejects `.`/`..` traversal so a stored
/// filename can never write outside the intended `files/<field>/<id>/` subtree
/// when the archive is later extracted (zip-slip). Falls back to `blob` when
/// nothing usable remains.
fn sanitize_archive_segment(name: &str) -> String {
    let last = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim();
    if last.is_empty() || last == "." || last == ".." {
        "blob".to_string()
    } else {
        last.to_string()
    }
}

/// Derive the bundle's data-file member name from the schema and the data
/// format's file extension (`csv` / `ndjson`).
///
/// Example: schema `Subject`, extension `csv` -> `Subject.csv`.
pub fn data_member_name(schema_name: &str, extension: &str) -> String {
    format!("{schema_name}.{extension}")
}

/// Collect every `file`-field blob that should be embedded in a `bundle_files`
/// export, as a flat list of [`FileBlobRef`]s.
///
/// Pure and total over the *already-stripped* `entities` and the resolved
/// `@exportable` `columns`: it walks only columns that are both `@exportable`
/// (present in `columns`) and a `file`-typed field on `schema`, so a `file`
/// field a caller may not read (stripped upstream) or that is not `@exportable`
/// never contributes a blob — the bundle can never widen what the flat export
/// already authorizes. Only attachments in the [`FileStatus::Available`] state
/// are bundled; pending/scanning/quarantined/rejected blobs are skipped so an
/// unscanned or quarantined object never leaves in an archive.
pub fn collect_file_blob_refs(
    entities: &[Entity],
    schema: &SchemaDefinition,
    columns: &[(String, Option<schema_forge_core::types::ExportFlatten>)],
) -> Vec<FileBlobRef> {
    let mut refs = Vec::new();
    for (field_name, _) in columns {
        let Some(field) = schema.field(field_name) else {
            continue;
        };
        if !matches!(field.field_type, FieldType::File(_)) {
            continue;
        }
        for entity in entities {
            let Some(value) = entity.field(field_name) else {
                continue;
            };
            for attachment in attachments_in(value) {
                if attachment.status != FileStatus::Available {
                    continue;
                }
                let member_name =
                    blob_member_name(field_name, entity.id.as_str(), &attachment.key);
                refs.push(FileBlobRef {
                    key: attachment.key,
                    member_name,
                });
            }
        }
    }
    refs
}

/// Decode the `FileAttachment`(s) stored in one entity field value.
///
/// A `file` field stores a single attachment object; an `array<file>` field
/// stores an array of them. Both are persisted as the flat attachment JSON shape
/// (not the tagged `DynamicValue` shape), matching the file-upload path, so a
/// `Json`/`Composite` object decodes directly and an `Array` decodes
/// element-by-element. Any value that does not parse as an attachment yields no
/// blobs — fail-closed.
fn attachments_in(value: &DynamicValue) -> Vec<FileAttachment> {
    match value {
        DynamicValue::Array(items) => items.iter().flat_map(attachments_in).collect(),
        other => attachment_json(other)
            .and_then(|json| serde_json::from_value::<FileAttachment>(json).ok())
            .into_iter()
            .collect(),
    }
}

/// Convert one entity-field `DynamicValue` back to the flat attachment JSON
/// object originally persisted, mirroring the file-upload path's decode. Returns
/// `None` for any shape that cannot represent a file attachment.
fn attachment_json(value: &DynamicValue) -> Option<serde_json::Value> {
    match value {
        DynamicValue::Json(v) => Some(v.clone()),
        DynamicValue::Composite(map) | DynamicValue::Map(map) => {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .filter_map(|(k, v)| scalar_json(v).map(|jv| (k.clone(), jv)))
                .collect();
            Some(serde_json::Value::Object(obj))
        }
        _ => None,
    }
}

/// Convert a scalar attachment-field value to JSON. Only the primitive shapes a
/// `FileAttachment` is built from are translated; anything else is dropped (the
/// resulting object simply omits that key and fails to deserialize if a required
/// key is missing — fail-closed).
fn scalar_json(value: &DynamicValue) -> Option<serde_json::Value> {
    match value {
        DynamicValue::Text(s) | DynamicValue::Enum(s) => Some(serde_json::Value::String(s.clone())),
        DynamicValue::Integer(n) => Some(serde_json::json!(*n)),
        DynamicValue::Float(n) => Some(serde_json::json!(*n)),
        DynamicValue::Boolean(b) => Some(serde_json::Value::Bool(*b)),
        DynamicValue::DateTime(dt) => Some(serde_json::Value::String(dt.to_rfc3339())),
        DynamicValue::Json(v) => Some(v.clone()),
        DynamicValue::Null => Some(serde_json::Value::Null),
        _ => None,
    }
}

/// Assemble `entries` into a single in-memory ZIP archive, returning its bytes.
///
/// Members are written in the order given, each Deflate-compressed. Pure over its
/// inputs (no storage, no clock): the archive is built entirely in an in-memory
/// cursor, so it is unit-testable and rides the buffered async path exactly as
/// XLSX does. A duplicate member name is an error rather than silently shadowing
/// an earlier member, so a name-collision bug surfaces loudly instead of dropping
/// data from the archive.
pub fn build_zip_bundle(entries: &[BundleEntry]) -> Result<Vec<u8>, String> {
    use std::collections::HashSet;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    let mut seen: HashSet<&str> = HashSet::with_capacity(entries.len());
    let mut writer = ZipWriter::new(Cursor::new(Vec::<u8>::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for entry in entries {
        if !seen.insert(entry.name.as_str()) {
            return Err(format!("duplicate bundle member '{}'", entry.name));
        }
        writer
            .start_file(entry.name.clone(), options)
            .map_err(|e| format!("zip start_file '{}' failed: {e}", entry.name))?;
        writer
            .write_all(&entry.bytes)
            .map_err(|e| format!("zip write '{}' failed: {e}", entry.name))?;
    }

    let cursor = writer
        .finish()
        .map_err(|e| format!("zip finish failed: {e}"))?;
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        Annotation, EntityId, ExportFormat, FieldAnnotation, FieldDefinition, FieldName, FieldType,
        FileAccess, FileConstraints, SchemaId, SchemaName, TextConstraints,
    };
    use std::collections::BTreeMap;
    use std::io::Read;

    fn read_zip(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut archive =
            zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).expect("valid zip");
        let mut out = Vec::new();
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).expect("member");
            let name = file.name().to_string();
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).expect("read member");
            out.push((name, buf));
        }
        out
    }

    #[test]
    fn build_bundle_preserves_members_and_order() {
        let entries = vec![
            BundleEntry::new("data.csv", b"name\nAda\n".to_vec()),
            BundleEntry::new("files/photo/e1/p.png", vec![0u8, 1, 2, 3]),
        ];
        let bytes = build_zip_bundle(&entries).unwrap();
        // A ZIP starts with the PK local-file signature.
        assert_eq!(&bytes[..2], b"PK");
        let members = read_zip(&bytes);
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].0, "data.csv");
        assert_eq!(members[0].1, b"name\nAda\n");
        assert_eq!(members[1].0, "files/photo/e1/p.png");
        assert_eq!(members[1].1, vec![0u8, 1, 2, 3]);
    }

    #[test]
    fn build_bundle_rejects_duplicate_member() {
        let entries = vec![
            BundleEntry::new("data.csv", b"a".to_vec()),
            BundleEntry::new("data.csv", b"b".to_vec()),
        ];
        let err = build_zip_bundle(&entries).unwrap_err();
        assert!(err.contains("duplicate"), "{err}");
    }

    #[test]
    fn empty_bundle_is_a_valid_empty_archive() {
        let bytes = build_zip_bundle(&[]).unwrap();
        let members = read_zip(&bytes);
        assert!(members.is_empty());
    }

    #[test]
    fn blob_member_name_layout_and_sanitization() {
        assert_eq!(
            blob_member_name("photo", "e1", "p.png"),
            "files/photo/e1/p.png"
        );
        // A storage key with prefixes collapses to its last segment.
        assert_eq!(
            blob_member_name("photo", "e1", "tenant/x/y/p.png"),
            "files/photo/e1/p.png"
        );
        // Traversal attempts are flattened, not honored (zip-slip defense).
        assert_eq!(
            blob_member_name("photo", "e1", "../../etc/passwd"),
            "files/photo/e1/passwd"
        );
        assert_eq!(blob_member_name("photo", "e1", ".."), "files/photo/e1/blob");
        assert_eq!(blob_member_name("photo", "e1", ""), "files/photo/e1/blob");
    }

    fn file_constraints() -> FileConstraints {
        FileConstraints {
            bucket: "documents".to_string(),
            max_size_bytes: 1_000_000,
            mime_allowlist: vec![],
            access: FileAccess::default(),
        }
    }

    fn file_field(name: &str) -> FieldDefinition {
        FieldDefinition::with_annotations(
            FieldName::new(name).unwrap(),
            FieldType::File(file_constraints()),
            vec![],
            vec![FieldAnnotation::Exportable { flatten: None }],
        )
    }

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition::with_annotations(
            FieldName::new(name).unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
            vec![],
            vec![FieldAnnotation::Exportable { flatten: None }],
        )
    }

    fn bundle_schema() -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Doc").unwrap(),
            vec![text_field("title"), file_field("attachment")],
            vec![Annotation::Export {
                formats: vec![ExportFormat::Zip],
                bundle_files: true,
                max_rows: 100,
            }],
        )
        .unwrap()
    }

    fn attachment_value(key: &str, status: FileStatus) -> DynamicValue {
        DynamicValue::Json(serde_json::json!({
            "key": key,
            "size": 3,
            "mime": "image/png",
            "status": status.as_str(),
            "created_at": "2024-01-01T00:00:00Z",
        }))
    }

    fn entity_with(id: &str, fields: Vec<(&str, DynamicValue)>) -> Entity {
        let mut map: BTreeMap<String, DynamicValue> = BTreeMap::new();
        for (k, v) in fields {
            map.insert(k.to_string(), v);
        }
        Entity::with_id(EntityId::new(id), SchemaName::new("Doc").unwrap(), map)
    }

    fn columns(
        schema: &SchemaDefinition,
    ) -> Vec<(String, Option<schema_forge_core::types::ExportFlatten>)> {
        schema
            .fields
            .iter()
            .filter_map(|f| f.exportable().map(|fl| (f.name.as_str().to_string(), fl)))
            .collect()
    }

    #[test]
    fn collect_refs_picks_available_file_blobs_only() {
        let schema = bundle_schema();
        let cols = columns(&schema);
        let available = entity_with(
            "e1",
            vec![
                ("title", DynamicValue::Text("t1".into())),
                ("attachment", attachment_value("blobs/a.png", FileStatus::Available)),
            ],
        );
        // The canonical entity id (EntityId::new normalizes the input) drives the
        // archive member path, so derive the expectation from it rather than the
        // literal seed.
        let expected_member = format!("files/attachment/{}/a.png", available.id.as_str());
        let entities = vec![
            available,
            // A quarantined blob must never be bundled.
            entity_with(
                "e2",
                vec![(
                    "attachment",
                    attachment_value("blobs/bad.png", FileStatus::Quarantined),
                )],
            ),
        ];
        let refs = collect_file_blob_refs(&entities, &schema, &cols);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].key, "blobs/a.png");
        assert_eq!(refs[0].member_name, expected_member);
    }

    #[test]
    fn collect_refs_skips_stripped_file_field() {
        // A row whose `attachment` field was stripped for read access (absent)
        // contributes no blob — the bundle never widens read access.
        let schema = bundle_schema();
        let cols = columns(&schema);
        let entities = vec![entity_with(
            "e1",
            vec![("title", DynamicValue::Text("t1".into()))],
        )];
        let refs = collect_file_blob_refs(&entities, &schema, &cols);
        assert!(refs.is_empty());
    }

    #[test]
    fn collect_refs_handles_array_of_files() {
        let schema = bundle_schema();
        let cols = columns(&schema);
        let entities = vec![entity_with(
            "e1",
            vec![(
                "attachment",
                DynamicValue::Array(vec![
                    attachment_value("blobs/a.png", FileStatus::Available),
                    attachment_value("blobs/b.png", FileStatus::Available),
                ]),
            )],
        )];
        let refs = collect_file_blob_refs(&entities, &schema, &cols);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].key, "blobs/a.png");
        assert_eq!(refs[1].key, "blobs/b.png");
    }

    #[test]
    fn data_member_name_uses_schema_and_extension() {
        assert_eq!(data_member_name("Subject", "csv"), "Subject.csv");
        assert_eq!(data_member_name("Subject", "ndjson"), "Subject.ndjson");
    }
}
