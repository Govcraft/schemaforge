//! Serve the embedded ops console (the `schemaforge-console` `dist`) same-origin
//! from `serve`, so a non-developer gets a working admin UI by opening the URL
//! the binary prints — no Node, no build, no CORS.
//!
//! Compiled only under the `embedded-console` feature. The console is a
//! runtime-generic SPA (it discovers schemas/CRUD at runtime), so one bundle
//! serves any schema. It is mounted as the router's `fallback`, so it only
//! handles paths the API/health routes don't (`mount_console` in `serve.rs`).
//!
//! The routing decision is factored into a pure `route_for` so it is unit-
//! testable without the embedded bytes (which are whatever `SCHEMAFORGE_CONSOLE_DIST`
//! held at build time — only `index.html` in a stub build).

use axum::{
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "$SCHEMAFORGE_CONSOLE_DIST"]
struct ConsoleAssets;

const INDEX: &str = "index.html";

/// The URL prefix the console is mounted under (see `serve::mount_console`).
const MOUNT: &str = "/console";

/// Strip the `/console` mount prefix to get the path *within* the SPA.
///
/// `/console` and `/console/` both map to the root (`/`); `/console/assets/x`
/// maps to `/assets/x`. A path outside the mount (shouldn't happen, since the
/// routes are scoped) maps to `/` so the worst case is the SPA shell.
fn strip_mount(path: &str) -> &str {
    match path.strip_prefix(MOUNT) {
        Some("") => "/",
        Some(rest) => rest,
        None => "/",
    }
}

/// What to serve for a request path, independent of the embedded bytes.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// Serve this exact embedded asset key.
    Asset(String),
    /// Serve `index.html` as the SPA history fallback (client-side route).
    Index,
    /// Nothing matched and no SPA fallback applies.
    NotFound,
}

/// Map a request path to a [`Route`], given a predicate for whether an asset key
/// exists and whether `index.html` is present.
///
/// - `/` (empty) resolves to `index.html`.
/// - An existing key is served directly.
/// - A missing key **under `assets/`** is a real 404 (a fingerprinted file that
///   genuinely isn't there) — never the HTML shell with a JS path.
/// - Any other missing path is a client-side route → `index.html` (SPA deep link),
///   when an index exists.
fn route_for(path: &str, exists: impl Fn(&str) -> bool, has_index: bool) -> Route {
    let trimmed = path.trim_start_matches('/');
    let key = if trimmed.is_empty() { INDEX } else { trimmed };

    if exists(key) {
        return Route::Asset(key.to_string());
    }
    if key.starts_with("assets/") {
        return Route::NotFound;
    }
    if has_index {
        Route::Index
    } else {
        Route::NotFound
    }
}

/// Cache policy for a served asset. Vite content-hashes everything under
/// `assets/`, so those are immutable; the HTML shell (index, or any SPA
/// fallback) must never be cached or a client pins a stale shell after upgrade.
fn cache_control(key: &str, is_index_fallback: bool) -> &'static str {
    if !is_index_fallback && key.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

/// Build the HTTP response for an embedded asset key.
fn asset_response(key: &str, bytes: Vec<u8>, is_index_fallback: bool) -> Response {
    let mime = mime_guess::from_path(key).first_or_octet_stream();
    (
        [
            (header::CONTENT_TYPE, mime.as_ref().to_string()),
            (
                header::CACHE_CONTROL,
                cache_control(key, is_index_fallback).to_string(),
            ),
        ],
        bytes,
    )
        .into_response()
}

/// Resolve a request path to a response over the embedded console assets.
pub fn serve_asset(path: &str) -> Response {
    let has_index = ConsoleAssets::get(INDEX).is_some();
    match route_for(path, |k| ConsoleAssets::get(k).is_some(), has_index) {
        Route::Asset(key) => {
            let data = ConsoleAssets::get(&key)
                .expect("asset existed in route_for predicate")
                .data
                .into_owned();
            asset_response(&key, data, false)
        }
        Route::Index => {
            let data = ConsoleAssets::get(INDEX)
                .expect("has_index implies index present")
                .data
                .into_owned();
            asset_response(INDEX, data, true)
        }
        Route::NotFound => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// axum handler for the `/console{,/,/*rest}` routes: strip the mount prefix
/// and serve the corresponding embedded asset (or the SPA shell).
pub async fn handler(uri: Uri) -> Response {
    serve_asset(strip_mount(uri.path()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_exists(key: &str) -> bool {
        matches!(key, "index.html" | "assets/app-abc123.js")
    }

    #[test]
    fn root_resolves_to_index() {
        assert_eq!(
            route_for("/", fake_exists, true),
            Route::Asset("index.html".to_string())
        );
    }

    #[test]
    fn existing_asset_is_served_directly() {
        assert_eq!(
            route_for("/assets/app-abc123.js", fake_exists, true),
            Route::Asset("assets/app-abc123.js".to_string())
        );
    }

    #[test]
    fn unknown_route_falls_back_to_index() {
        // A client-side deep link (no such file) → SPA shell.
        assert_eq!(route_for("/Contact/abc", fake_exists, true), Route::Index);
    }

    #[test]
    fn missing_asset_is_not_found_not_index() {
        // A fingerprinted asset that isn't there must 404, not return HTML.
        assert_eq!(
            route_for("/assets/missing.js", fake_exists, true),
            Route::NotFound
        );
    }

    #[test]
    fn no_index_means_not_found() {
        assert_eq!(route_for("/anything", |_| false, false), Route::NotFound);
    }

    #[test]
    fn strip_mount_maps_console_prefix_to_spa_path() {
        assert_eq!(strip_mount("/console"), "/");
        assert_eq!(strip_mount("/console/"), "/");
        assert_eq!(strip_mount("/console/assets/app-abc123.js"), "/assets/app-abc123.js");
        assert_eq!(strip_mount("/console/Contact/abc"), "/Contact/abc");
    }

    #[test]
    fn assets_get_immutable_cache_html_does_not() {
        assert_eq!(
            cache_control("assets/app-abc123.js", false),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(cache_control("index.html", false), "no-cache");
        // An SPA fallback that happens to resolve a non-asset key is still HTML.
        assert_eq!(cache_control("index.html", true), "no-cache");
    }
}
