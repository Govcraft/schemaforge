//! The cel-spec `encoders` extension: `base64.encode` / `base64.decode`.
//!
//! - `base64.encode(bytes) -> string` — standard Base64 WITH padding.
//! - `base64.decode(string) -> bytes` — standard (padded) Base64; invalid input
//!   is a runtime evaluation error (fail-closed, never a panic).
//!
//! The codec is shared with the rest of SchemaForge via
//! [`schema_forge_core::types::base64`], so a `bytes` value encoded by a rule
//! decodes identically on the REST wire and in the storage backends.

use schema_forge_core::types::base64 as core_base64;

use crate::error::EvalError;
use crate::value::CelValue;

/// `base64.encode(bytes) -> string`.
pub fn encode(x: &CelValue) -> Result<CelValue, EvalError> {
    match x {
        CelValue::Bytes(b) => Ok(CelValue::String(core_base64::encode_standard(b))),
        _ => Err(EvalError::new("no such overload")),
    }
}

/// `base64.decode(string) -> bytes`.
///
/// Per the cel-spec `encoders` extension, both padded (`aGVsbG8=`) and unpadded
/// (`aGVsbG8`) standard-alphabet input is accepted. Returns an evaluation error
/// (not a panic) when the string is not valid Base64.
pub fn decode(x: &CelValue) -> Result<CelValue, EvalError> {
    match x {
        CelValue::String(s) => core_base64::decode_standard_indifferent(s)
            .map(CelValue::Bytes)
            .map_err(|e| EvalError::new(format!("invalid base64: {e}"))),
        _ => Err(EvalError::new("no such overload")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_known_vector() {
        let got = encode(&CelValue::Bytes(b"hello".to_vec())).unwrap();
        assert_eq!(got, CelValue::String("aGVsbG8=".into()));
    }

    #[test]
    fn decode_known_vector() {
        let got = decode(&CelValue::String("aGVsbG8=".into())).unwrap();
        assert_eq!(got, CelValue::Bytes(b"hello".to_vec()));
    }

    #[test]
    fn round_trip() {
        let original = CelValue::Bytes(vec![0x00, 0x10, 0xff, 0xfe, 0x42]);
        let CelValue::String(s) = encode(&original).unwrap() else {
            panic!("encode should yield a string");
        };
        assert_eq!(decode(&CelValue::String(s)).unwrap(), original);
    }

    #[test]
    fn decode_accepts_unpadded_per_cel_spec() {
        let got = decode(&CelValue::String("aGVsbG8".into())).unwrap();
        assert_eq!(got, CelValue::Bytes(b"hello".to_vec()));
    }

    #[test]
    fn decode_invalid_is_eval_error_not_panic() {
        let err = decode(&CelValue::String("not valid base64!!".into())).unwrap_err();
        assert!(
            err.message().contains("invalid base64"),
            "unexpected error: {}",
            err.message()
        );
    }

    #[test]
    fn encode_non_bytes_is_no_such_overload() {
        assert_eq!(
            encode(&CelValue::Int(1)).unwrap_err().message(),
            "no such overload"
        );
    }

    #[test]
    fn decode_non_string_is_no_such_overload() {
        assert_eq!(
            decode(&CelValue::Int(1)).unwrap_err().message(),
            "no such overload"
        );
    }
}
