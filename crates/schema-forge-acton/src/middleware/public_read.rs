//! Select HTTP reads whose handlers authorize anonymous callers with Cedar.

use axum::http::Method;

/// Whether missing bearer credentials may proceed to entity authorization.
///
/// Only collection and individual-entity GET/HEAD requests qualify. This does
/// not grant access: the entity handlers evaluate the active Cedar policies.
pub fn allows_missing_credentials(method: &Method, path: &str) -> bool {
    if method != Method::GET && method != Method::HEAD {
        return false;
    }
    let Some(rest) = path.strip_prefix("/api/v1/forge/schemas/") else {
        return false;
    };
    let mut segments = rest.split('/');
    if segments.next().is_none_or(|schema| schema.is_empty())
        || segments.next() != Some("entities")
    {
        return false;
    }
    match segments.next() {
        None => true,
        Some(id) => !id.is_empty() && segments.next().is_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_entity_list_and_detail_reads_allow_missing_credentials() {
        for method in [Method::GET, Method::HEAD] {
            for path in [
                "/api/v1/forge/schemas/Branding/entities",
                "/api/v1/forge/schemas/Branding/entities/branding_123",
            ] {
                assert!(allows_missing_credentials(&method, path));
            }
            for path in [
                "/api/v1/forge/schemas",
                "/api/v1/forge/schemas/Branding",
                "/api/v1/forge/schemas/Branding/entities/branding_123/history",
                "/api/v1/forge/schemas//entities",
                "/api/v1/forge/schemas/Branding/entities/",
            ] {
                assert!(!allows_missing_credentials(&method, path));
            }
        }
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert!(!allows_missing_credentials(
                &method,
                "/api/v1/forge/schemas/Branding/entities"
            ));
        }
    }
}
