//! Standard (padded) Base64 codec shared across SchemaForge layers.
//!
//! A SchemaForge `bytes` value is inline binary carried at runtime as a
//! `Vec<u8>`. Its canonical *string* form — used on the REST wire, in
//! [`DynamicValue::Bytes`]'s [`Display`], and when projecting a stored `bytes`
//! value into JSON — is **standard Base64 with padding** (the RFC 4648 §4
//! alphabet, `=`-padded), matching the cel-spec encoders extension's
//! `base64.encode`/`base64.decode` and SurrealDB's `<bytes>` literal form.
//!
//! These are pure functions with no I/O so the alphabet, padding, and
//! invalid-input behaviour are exhaustively testable without any backend.
//!
//! [`DynamicValue::Bytes`]: super::DynamicValue::Bytes
//! [`Display`]: std::fmt::Display

use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig, STANDARD};
use base64::engine::DecodePaddingMode;
use base64::Engine as _;

/// Standard alphabet, padding-indifferent decoder: accepts input with or without
/// trailing `=` padding. Used by the CEL `base64.decode` builtin, whose cel-spec
/// `encoders` extension semantics accept unpadded input (e.g. `aGVsbG8`).
const STANDARD_INDIFFERENT: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// The input string was not valid standard (padded) Base64.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Base64DecodeError {
    /// Human-readable detail from the underlying decoder.
    detail: String,
}

impl Base64DecodeError {
    /// The decoder's detail message.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl std::fmt::Display for Base64DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid standard base64 input: {}", self.detail)
    }
}

impl std::error::Error for Base64DecodeError {}

/// Encode bytes as standard Base64 with padding.
#[must_use]
pub fn encode_standard(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// Decode a standard (padded) Base64 string into bytes.
///
/// # Errors
/// Returns [`Base64DecodeError`] when the input is not valid standard,
/// `=`-padded Base64 (bad alphabet, wrong padding, or truncated input). Fails
/// closed: never panics, never returns partial output.
pub fn decode_standard(s: &str) -> Result<Vec<u8>, Base64DecodeError> {
    STANDARD.decode(s).map_err(|e| Base64DecodeError {
        detail: e.to_string(),
    })
}

/// Decode a standard-alphabet Base64 string, tolerating present-or-absent
/// trailing `=` padding.
///
/// This matches the cel-spec `encoders` extension's `base64.decode`, which
/// accepts both `aGVsbG8=` and `aGVsbG8`. Still fails closed: a bad alphabet or
/// otherwise malformed input is an error, never a panic or partial output.
///
/// # Errors
/// Returns [`Base64DecodeError`] when the input is not valid standard-alphabet
/// Base64 (independent of padding).
pub fn decode_standard_indifferent(s: &str) -> Result<Vec<u8>, Base64DecodeError> {
    STANDARD_INDIFFERENT
        .decode(s)
        .map_err(|e| Base64DecodeError {
            detail: e.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_known_vector() {
        assert_eq!(encode_standard(b"hello"), "aGVsbG8=");
        assert_eq!(encode_standard(b""), "");
        assert_eq!(encode_standard(b"f"), "Zg==");
    }

    #[test]
    fn decode_known_vector() {
        assert_eq!(decode_standard("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_standard("").unwrap(), Vec::<u8>::new());
        assert_eq!(decode_standard("Zg==").unwrap(), b"f");
    }

    #[test]
    fn roundtrip_arbitrary_bytes() {
        let bytes: Vec<u8> = (0u8..=255).collect();
        let encoded = encode_standard(&bytes);
        assert_eq!(decode_standard(&encoded).unwrap(), bytes);
    }

    #[test]
    fn decode_rejects_invalid_alphabet() {
        let err = decode_standard("!!!!").unwrap_err();
        assert!(err.to_string().contains("invalid standard base64 input"));
    }

    #[test]
    fn decode_rejects_unpadded_when_padding_required() {
        // Standard engine requires canonical padding; "aGVsbG8" (no `=`) is rejected.
        assert!(decode_standard("aGVsbG8").is_err());
    }

    #[test]
    fn decode_indifferent_accepts_padded_and_unpadded() {
        assert_eq!(decode_standard_indifferent("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_standard_indifferent("aGVsbG8").unwrap(), b"hello");
    }

    #[test]
    fn decode_indifferent_still_rejects_bad_alphabet() {
        assert!(decode_standard_indifferent("!!!!").is_err());
    }
}
