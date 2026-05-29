//! String predicate built-ins: `contains`, `startsWith`, `endsWith`, `matches`.
//!
//! These are pure `&str` predicates returning a `bool` [`CelValue`]. `matches`
//! uses the `regex` crate, whose RE2-style engine runs in guaranteed linear time
//! (no catastrophic backtracking / ReDoS) — the right fit for the evaluator's
//! guaranteed-terminating, DoS-hardened posture. Per CEL semantics, `matches` is
//! an UNANCHORED search (RE2 `PartialMatch`), i.e. it succeeds if the pattern
//! matches anywhere in the subject, which is exactly `Regex::is_match`.

use regex::Regex;

use crate::error::EvalError;
use crate::value::CelValue;

fn overload() -> EvalError {
    EvalError::new("no such overload")
}

/// `s.contains(sub)`.
pub fn contains(recv: &CelValue, arg: &CelValue) -> Result<CelValue, EvalError> {
    let (s, sub) = two_strings(recv, arg)?;
    Ok(CelValue::Bool(s.contains(sub)))
}

/// `s.startsWith(prefix)`.
pub fn starts_with(recv: &CelValue, arg: &CelValue) -> Result<CelValue, EvalError> {
    let (s, p) = two_strings(recv, arg)?;
    Ok(CelValue::Bool(s.starts_with(p)))
}

/// `s.endsWith(suffix)`.
pub fn ends_with(recv: &CelValue, arg: &CelValue) -> Result<CelValue, EvalError> {
    let (s, p) = two_strings(recv, arg)?;
    Ok(CelValue::Bool(s.ends_with(p)))
}

/// `s.matches(re)` / `matches(s, re)`: UNANCHORED regex search.
pub fn matches(subject: &CelValue, pattern: &CelValue) -> Result<CelValue, EvalError> {
    let (s, re) = two_strings(subject, pattern)?;
    let compiled = Regex::new(re).map_err(|e| EvalError::new(format!("invalid regex: {e}")))?;
    Ok(CelValue::Bool(compiled.is_match(s)))
}

/// Extract two string operands, or report a missing overload.
fn two_strings<'a>(a: &'a CelValue, b: &'a CelValue) -> Result<(&'a str, &'a str), EvalError> {
    match (a, b) {
        (CelValue::String(x), CelValue::String(y)) => Ok((x, y)),
        _ => Err(overload()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> CelValue {
        CelValue::String(v.into())
    }

    #[test]
    fn contains_starts_ends() {
        assert_eq!(
            contains(&s("hello"), &s("he")).unwrap(),
            CelValue::Bool(true)
        );
        assert_eq!(
            contains(&s("hello"), &s("ol")).unwrap(),
            CelValue::Bool(false)
        );
        assert_eq!(contains(&s("hello"), &s("")).unwrap(), CelValue::Bool(true));
        assert_eq!(
            starts_with(&s("foobar"), &s("foo")).unwrap(),
            CelValue::Bool(true)
        );
        assert_eq!(
            ends_with(&s("foobar"), &s("foo")).unwrap(),
            CelValue::Bool(false)
        );
        assert_eq!(
            ends_with(&s("forté"), &s("té")).unwrap(),
            CelValue::Bool(true)
        );
    }

    #[test]
    fn matches_is_unanchored() {
        // 'ubb' matches inside 'hubba' (an anchored match would fail).
        assert_eq!(
            matches(&s("hubba"), &s("ubb")).unwrap(),
            CelValue::Bool(true)
        );
        assert_eq!(matches(&s("abcd"), &s("bc")).unwrap(), CelValue::Bool(true));
        assert_eq!(
            matches(&s("grey"), &s("gr(a|e)y")).unwrap(),
            CelValue::Bool(true)
        );
        assert_eq!(matches(&s("cows"), &s("")).unwrap(), CelValue::Bool(true));
        assert_eq!(
            matches(&s(""), &s("foo|bar")).unwrap(),
            CelValue::Bool(false)
        );
        assert_eq!(
            matches(&s("mañana"), &s("a+ñ+a+")).unwrap(),
            CelValue::Bool(true)
        );
    }

    #[test]
    fn invalid_regex_errors() {
        assert!(matches(&s("x"), &s("(")).is_err());
    }
}
