use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::SchemaError;

/// Constraints applied to a `file`-typed field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileConstraints {
    /// Name of the configured storage backend ("bucket") that holds this field's
    /// uploaded objects. Must resolve to a `[schema_forge.storage.backends.<name>]`
    /// entry at runtime.
    pub bucket: String,
    /// Maximum byte size accepted at upload-mint time.
    pub max_size_bytes: u64,
    /// Allowed MIME types. Wildcards (`image/*`) are supported.
    pub mime_allowlist: Vec<MimePattern>,
    /// How downloads are served.
    #[serde(default)]
    pub access: FileAccess,
}

/// A single entry in a file field's MIME allowlist.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum MimePattern {
    /// Exact match, e.g. `application/pdf`.
    Exact(String),
    /// Family match stored as the type prefix only, e.g. `image` from `image/*`.
    Family(String),
}

impl MimePattern {
    /// Parses a user-facing MIME pattern string (`application/pdf`, `image/*`).
    pub fn parse(raw: &str) -> Result<Self, SchemaError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(SchemaError::InvalidMimePattern(raw.to_string()));
        }
        let Some((kind, subtype)) = trimmed.split_once('/') else {
            return Err(SchemaError::InvalidMimePattern(raw.to_string()));
        };
        if kind.is_empty() || subtype.is_empty() {
            return Err(SchemaError::InvalidMimePattern(raw.to_string()));
        }
        if subtype == "*" {
            Ok(Self::Family(kind.to_string()))
        } else {
            Ok(Self::Exact(format!("{kind}/{subtype}")))
        }
    }

    /// Returns true when the pattern matches a concrete MIME string.
    pub fn matches(&self, mime: &str) -> bool {
        match self {
            Self::Exact(m) => m.eq_ignore_ascii_case(mime),
            Self::Family(prefix) => match mime.split_once('/') {
                Some((m_kind, _)) => m_kind.eq_ignore_ascii_case(prefix),
                None => false,
            },
        }
    }
}

impl fmt::Display for MimePattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact(m) => f.write_str(m),
            Self::Family(prefix) => write!(f, "{prefix}/*"),
        }
    }
}

/// How clients retrieve file bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FileAccess {
    /// The runtime mints a short-TTL presigned GET URL and the client reads directly
    /// from the storage backend. Default.
    #[default]
    Presigned,
    /// The runtime proxies bytes from the storage backend on every request, so
    /// authorization is re-checked at fetch time. Slower but higher assurance.
    Proxied,
}

impl FileAccess {
    /// Machine-readable name used in DSL and serde.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Presigned => "presigned",
            Self::Proxied => "proxied",
        }
    }
}

impl fmt::Display for FileAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FileAccess {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "presigned" => Ok(Self::Presigned),
            "proxied" => Ok(Self::Proxied),
            _ => Err(()),
        }
    }
}

/// Lifecycle state of an uploaded file. Downloads are refused unless `Available`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    /// Upload URL has been minted; bytes not yet confirmed.
    Pending,
    /// Confirmed present in storage; scan dispatch pending.
    Uploaded,
    /// Scan in progress; not yet downloadable.
    Scanning,
    /// Fully available for download.
    Available,
    /// Scan failed; bytes quarantined, never downloadable.
    Quarantined,
    /// Validation or scan rejected the upload outright.
    Rejected,
}

impl FileStatus {
    /// All statuses in lifecycle order.
    pub const ALL: &'static [FileStatus] = &[
        Self::Pending,
        Self::Uploaded,
        Self::Scanning,
        Self::Available,
        Self::Quarantined,
        Self::Rejected,
    ];

    /// Machine-readable name used in DSL and serde.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Uploaded => "uploaded",
            Self::Scanning => "scanning",
            Self::Available => "available",
            Self::Quarantined => "quarantined",
            Self::Rejected => "rejected",
        }
    }
}

impl fmt::Display for FileStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FileStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "uploaded" => Ok(Self::Uploaded),
            "scanning" => Ok(Self::Scanning),
            "available" => Ok(Self::Available),
            "quarantined" => Ok(Self::Quarantined),
            "rejected" => Ok(Self::Rejected),
            _ => Err(()),
        }
    }
}

/// Persisted metadata for a single uploaded file, stored in the JSONB column that
/// backs a `file` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileAttachment {
    /// Object key within the backend's bucket.
    pub key: String,
    /// Size in bytes (client-declared at mint, re-verified via HeadObject on confirm).
    pub size: u64,
    /// Content-Type declared at mint time.
    pub mime: String,
    /// Hex-encoded SHA-256 of the bytes, set at confirm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
    /// Lifecycle state.
    pub status: FileStatus,
    /// Wall-clock time the upload-URL was minted.
    pub created_at: DateTime<Utc>,
    /// Wall-clock time the upload was confirmed present in storage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uploaded_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_parse_exact() {
        let p = MimePattern::parse("application/pdf").unwrap();
        assert_eq!(p, MimePattern::Exact("application/pdf".into()));
    }

    #[test]
    fn mime_parse_family() {
        let p = MimePattern::parse("image/*").unwrap();
        assert_eq!(p, MimePattern::Family("image".into()));
    }

    #[test]
    fn mime_parse_trims_whitespace() {
        let p = MimePattern::parse("  image/*  ").unwrap();
        assert_eq!(p, MimePattern::Family("image".into()));
    }

    #[test]
    fn mime_parse_empty_rejected() {
        assert!(matches!(
            MimePattern::parse(""),
            Err(SchemaError::InvalidMimePattern(_))
        ));
        assert!(matches!(
            MimePattern::parse("   "),
            Err(SchemaError::InvalidMimePattern(_))
        ));
    }

    #[test]
    fn mime_parse_missing_slash_rejected() {
        assert!(matches!(
            MimePattern::parse("applicationpdf"),
            Err(SchemaError::InvalidMimePattern(_))
        ));
    }

    #[test]
    fn mime_parse_missing_subtype_rejected() {
        assert!(matches!(
            MimePattern::parse("image/"),
            Err(SchemaError::InvalidMimePattern(_))
        ));
    }

    #[test]
    fn mime_parse_missing_type_rejected() {
        assert!(matches!(
            MimePattern::parse("/pdf"),
            Err(SchemaError::InvalidMimePattern(_))
        ));
    }

    #[test]
    fn mime_exact_matches_are_case_insensitive() {
        let p = MimePattern::Exact("application/pdf".into());
        assert!(p.matches("application/pdf"));
        assert!(p.matches("APPLICATION/PDF"));
        assert!(!p.matches("application/json"));
        assert!(!p.matches("application/pdfX"));
    }

    #[test]
    fn mime_family_matches_prefix() {
        let p = MimePattern::Family("image".into());
        assert!(p.matches("image/png"));
        assert!(p.matches("IMAGE/jpeg"));
        assert!(!p.matches("video/mp4"));
        assert!(!p.matches("imagepng"));
    }

    #[test]
    fn mime_display_roundtrip() {
        assert_eq!(
            MimePattern::Exact("application/pdf".into()).to_string(),
            "application/pdf"
        );
        assert_eq!(MimePattern::Family("image".into()).to_string(), "image/*");
    }

    #[test]
    fn file_access_default_is_presigned() {
        assert_eq!(FileAccess::default(), FileAccess::Presigned);
    }

    #[test]
    fn file_access_str_roundtrip() {
        for access in [FileAccess::Presigned, FileAccess::Proxied] {
            assert_eq!(FileAccess::from_str(access.as_str()), Ok(access));
        }
        assert!(FileAccess::from_str("other").is_err());
    }

    #[test]
    fn file_status_str_roundtrip_all() {
        for status in FileStatus::ALL {
            assert_eq!(FileStatus::from_str(status.as_str()), Ok(*status));
            assert_eq!(status.to_string(), status.as_str());
        }
    }

    #[test]
    fn file_status_rejects_unknown() {
        assert!(FileStatus::from_str("bogus").is_err());
    }

    #[test]
    fn constraints_serde_roundtrip() {
        let c = FileConstraints {
            bucket: "documents".into(),
            max_size_bytes: 25 * 1024 * 1024,
            mime_allowlist: vec![
                MimePattern::Exact("application/pdf".into()),
                MimePattern::Family("image".into()),
            ],
            access: FileAccess::Presigned,
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: FileConstraints = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn attachment_serde_roundtrip_minimal() {
        let created_at = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let a = FileAttachment {
            key: "docs/tenant-a/ent-123/contract/01HX/contract.pdf".into(),
            size: 1_048_576,
            mime: "application/pdf".into(),
            checksum: None,
            status: FileStatus::Pending,
            created_at,
            uploaded_at: None,
        };
        let json = serde_json::to_string(&a).unwrap();
        assert!(!json.contains("checksum"), "checksum should be skipped when None");
        assert!(!json.contains("uploaded_at"), "uploaded_at should be skipped when None");
        let back: FileAttachment = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn attachment_serde_roundtrip_full() {
        let created_at = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let uploaded_at = DateTime::<Utc>::from_timestamp(1_700_000_030, 0).unwrap();
        let a = FileAttachment {
            key: "evidence/tenant-a/ent-9/image/01HX/photo.jpg".into(),
            size: 512,
            mime: "image/jpeg".into(),
            checksum: Some("abc123".into()),
            status: FileStatus::Available,
            created_at,
            uploaded_at: Some(uploaded_at),
        };
        let json = serde_json::to_string(&a).unwrap();
        let back: FileAttachment = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }
}
