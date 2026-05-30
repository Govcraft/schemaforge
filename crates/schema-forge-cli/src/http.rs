//! HTTP client for the entity REST API of a running SchemaForge instance.
//!
//! This is the only module that knows the wire format: the versioned base
//! path, the Bearer-token header, the `{ "fields": ... }` request envelope,
//! and the `{ "error": kind, "message": ... }` error envelope. The `entity`
//! and `login` command handlers build a [`ForgeClient`] and call its typed
//! methods; they never touch `reqwest` directly.
//!
//! TLS is rustls, routed through the process-default crypto provider that
//! `main.rs` installs at startup (`install_default_crypto_provider`) — under
//! the `fips` feature that is the FIPS-validated aws-lc-rs provider, so the
//! entity client stays consistent with the rest of the workspace. System
//! roots come from `rustls-platform-verifier`; a private-PKI root can be
//! added with `--ca-cert`.

use reqwest::{Client, Method, Url};
use serde::Deserialize;
use serde_json::Value;

use crate::config::ResolvedClient;
use crate::error::CliError;
use crate::output::OutputContext;

/// Client-side view of the server's `EntityResponse`. Unknown fields
/// (`schema`, `permissions`) are ignored — only what the renderer needs.
#[derive(Debug, Deserialize)]
pub struct EntityDto {
    /// The entity ID (TypeID).
    pub id: String,
    /// The entity's fields, including any `field__display` relation labels.
    #[serde(default)]
    pub fields: serde_json::Map<String, Value>,
}

/// Client-side view of the server's `ListEntitiesResponse`.
#[derive(Debug, Deserialize)]
pub struct ListDto {
    /// The entities in this page.
    #[serde(default)]
    pub entities: Vec<EntityDto>,
    /// Number of entities in this response.
    #[serde(default)]
    pub count: usize,
    /// Total matching entities before pagination, when computed.
    #[serde(default)]
    pub total_count: Option<usize>,
}

/// Build the versioned API base, e.g.
/// `https://host/api/v1/forge` from `https://host` + `v1`.
///
/// Pure: trims a trailing slash from the origin and joins the conventional
/// `/api/{version}/forge` path. (The default `forge` route prefix; custom
/// prefixes are out of scope for v1.)
pub fn forge_base(server: &str, api_version: &str) -> String {
    format!(
        "{}/api/{}/forge",
        server.trim_end_matches('/'),
        api_version
    )
}

/// Translate a non-success HTTP response into an actionable [`CliError`].
///
/// Parses the server's `{ "error": kind, "message": ..., "details": [...] }`
/// envelope (falling back to the raw body), prefixes a status-specific
/// explanation, folds in any validation `details`, and — for 401 — appends a
/// tip pointing at the auth flow. Pure: no IO, fully unit-testable.
pub fn classify_http_error(status: u16, body: &str) -> CliError {
    let parsed: Option<Value> = serde_json::from_str(body).ok();

    let kind = parsed
        .as_ref()
        .and_then(|v| v.get("error"))
        .and_then(Value::as_str)
        .unwrap_or("http_error")
        .to_string();

    let server_msg = parsed
        .as_ref()
        .and_then(|v| v.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let details: Vec<String> = parsed
        .as_ref()
        .and_then(|v| v.get("details"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|d| match d {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect()
        })
        .unwrap_or_default();

    let base = server_msg.unwrap_or_else(|| {
        if body.trim().is_empty() {
            format!("HTTP {status}")
        } else {
            body.trim().to_string()
        }
    });

    let mut message = match status {
        401 => format!(
            "authentication failed (HTTP 401): {base}\n  tip: run `schemaforge login`, \
             or pass --token-file / --token-stdin / set SCHEMAFORGE_TOKEN"
        ),
        403 => format!("permission denied (HTTP 403): {base}"),
        404 => format!("not found (HTTP 404): {base}"),
        409 => format!("conflict (HTTP 409): {base}"),
        422 => format!("validation failed (HTTP 422): {base}"),
        s if (500..=599).contains(&s) => format!("server error (HTTP {status}): {base}"),
        _ => format!("request failed (HTTP {status}): {base}"),
    };

    if !details.is_empty() {
        message.push_str("\n  details:");
        for d in &details {
            message.push_str("\n    - ");
            message.push_str(d);
        }
    }

    CliError::Http {
        status,
        kind,
        message,
    }
}

/// HTTP client bound to one running instance and (optionally) one token.
pub struct ForgeClient {
    http: Client,
    base: String,
    token: Option<String>,
}

impl ForgeClient {
    /// Build a client from resolved connection settings.
    ///
    /// Emits a loud warning for `--insecure`, loads a `--ca-cert` root if
    /// present, and forces the rustls backend so the process-default crypto
    /// provider is used regardless of other TLS features in the dependency
    /// graph.
    pub fn new(rc: &ResolvedClient, output: &OutputContext) -> Result<Self, CliError> {
        if rc.insecure {
            output.warn(
                "TLS certificate verification is DISABLED (--insecure). \
                 Never use this against production.",
            );
        }

        let mut builder = Client::builder().use_rustls_tls().timeout(rc.timeout);

        if rc.insecure {
            builder = builder.danger_accept_invalid_certs(true);
        }

        if let Some(ca) = &rc.ca_cert {
            let pem = std::fs::read(ca).map_err(|e| CliError::Io {
                path: ca.clone(),
                source: e,
            })?;
            let cert = reqwest::Certificate::from_pem(&pem).map_err(|e| CliError::Config {
                message: format!("invalid CA certificate {}: {e}", ca.display()),
            })?;
            builder = builder.add_root_certificate(cert);
        }

        let http = builder.build().map_err(|e| CliError::Config {
            message: format!("failed to build HTTP client: {e}"),
        })?;

        Ok(Self {
            http,
            base: forge_base(&rc.server, &rc.api_version),
            token: rc.token.clone(),
        })
    }

    /// Join the versioned base with API-relative path segments, percent-
    /// encoding each segment so schema names and entity IDs are safe.
    fn url(&self, segments: &[&str]) -> Result<Url, CliError> {
        let mut url = Url::parse(&self.base).map_err(|e| CliError::Config {
            message: format!("invalid server URL '{}': {e}", self.base),
        })?;
        {
            let mut path = url.path_segments_mut().map_err(|()| CliError::Config {
                message: format!("server URL '{}' cannot be a base", self.base),
            })?;
            path.extend(segments);
        }
        Ok(url)
    }

    /// Send a request and decode the response.
    ///
    /// Returns `Ok(None)` for a 2xx with no body (e.g. 204 on delete),
    /// `Ok(Some(json))` for a 2xx with a JSON body, a [`CliError::Connection`]
    /// when the request never reached a response, and the mapped
    /// [`CliError::Http`] for any non-2xx status.
    async fn send(
        &self,
        method: Method,
        mut url: Url,
        query: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<Option<Value>, CliError> {
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        }
        let mut req = self.http.request(method, url);
        if let Some(tok) = &self.token {
            req = req.bearer_auth(tok);
        }
        if let Some(b) = body {
            req = req.json(b);
        }

        let resp = req.send().await.map_err(|e| CliError::Connection {
            message: e.to_string(),
        })?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| CliError::Connection {
            message: e.to_string(),
        })?;

        if status.is_success() {
            if text.trim().is_empty() {
                return Ok(None);
            }
            let value = serde_json::from_str(&text).map_err(|e| CliError::Server {
                message: format!("invalid JSON in server response: {e}"),
            })?;
            Ok(Some(value))
        } else {
            Err(classify_http_error(status.as_u16(), &text))
        }
    }

    // --- Entity operations -------------------------------------------------

    /// `GET /schemas/{schema}/entities` with query parameters.
    pub async fn list(
        &self,
        schema: &str,
        query: &[(String, String)],
    ) -> Result<Value, CliError> {
        let url = self.url(&["schemas", schema, "entities"])?;
        self.send(Method::GET, url, query, None)
            .await
            .map(|v| v.unwrap_or(Value::Null))
    }

    /// `POST /schemas/{schema}/entities/query` with a JSON query body.
    pub async fn query(&self, schema: &str, body: &Value) -> Result<Value, CliError> {
        let url = self.url(&["schemas", schema, "entities", "query"])?;
        self.send(Method::POST, url, &[], Some(body))
            .await
            .map(|v| v.unwrap_or(Value::Null))
    }

    /// `GET /schemas/{schema}/entities/{id}`.
    pub async fn get(
        &self,
        schema: &str,
        id: &str,
        query: &[(String, String)],
    ) -> Result<Value, CliError> {
        let url = self.url(&["schemas", schema, "entities", id])?;
        self.send(Method::GET, url, query, None)
            .await
            .map(|v| v.unwrap_or(Value::Null))
    }

    /// `POST /schemas/{schema}/entities` (create).
    pub async fn create(&self, schema: &str, body: &Value) -> Result<Value, CliError> {
        let url = self.url(&["schemas", schema, "entities"])?;
        self.send(Method::POST, url, &[], Some(body))
            .await
            .map(|v| v.unwrap_or(Value::Null))
    }

    /// `PUT /schemas/{schema}/entities/{id}` (replace).
    pub async fn replace(&self, schema: &str, id: &str, body: &Value) -> Result<Value, CliError> {
        let url = self.url(&["schemas", schema, "entities", id])?;
        self.send(Method::PUT, url, &[], Some(body))
            .await
            .map(|v| v.unwrap_or(Value::Null))
    }

    /// `PATCH /schemas/{schema}/entities/{id}` (partial update).
    pub async fn patch(&self, schema: &str, id: &str, body: &Value) -> Result<Value, CliError> {
        let url = self.url(&["schemas", schema, "entities", id])?;
        self.send(Method::PATCH, url, &[], Some(body))
            .await
            .map(|v| v.unwrap_or(Value::Null))
    }

    /// `DELETE /schemas/{schema}/entities/{id}`.
    pub async fn delete(&self, schema: &str, id: &str) -> Result<(), CliError> {
        let url = self.url(&["schemas", schema, "entities", id])?;
        self.send(Method::DELETE, url, &[], None).await.map(|_| ())
    }

    /// `POST /auth/login` — exchange credentials for a token. Unauthenticated.
    pub async fn login(&self, username: &str, password: &str) -> Result<Value, CliError> {
        let url = self.url(&["auth", "login"])?;
        let body = serde_json::json!({ "username": username, "password": password });
        self.send(Method::POST, url, &[], Some(&body))
            .await
            .map(|v| v.unwrap_or(Value::Null))
    }

    // --- Export operations -------------------------------------------------

    /// `POST /schemas/{schema}/entities/export` with a `{ filter?, fields?,
    /// format, async? }` body.
    ///
    /// The endpoint answers in one of two shapes (ADR-0003), distinguished by
    /// the response `Content-Type`:
    /// - a **streamed file** (`200 OK`, `text/csv` / `application/x-ndjson`),
    ///   returned as [`ExportResponse::File`]; or
    /// - an **async job** (`202 Accepted`, `application/json`), returned as
    ///   [`ExportResponse::Job`] carrying the job-status JSON.
    ///
    /// Streaming the bytes (rather than buffering to a JSON value) keeps a large
    /// CSV/NDJSON export off the heap-as-`Value` path the other verbs use.
    pub async fn export(&self, schema: &str, body: &Value) -> Result<ExportResponse, CliError> {
        let url = self.url(&["schemas", schema, "entities", "export"])?;
        let mut req = self.http.request(Method::POST, url).json(body);
        if let Some(tok) = &self.token {
            req = req.bearer_auth(tok);
        }

        let resp = req.send().await.map_err(|e| CliError::Connection {
            message: e.to_string(),
        })?;

        let status = resp.status();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let filename = content_disposition_filename(
            resp.headers()
                .get(reqwest::header::CONTENT_DISPOSITION)
                .and_then(|v| v.to_str().ok()),
        );

        let bytes = resp.bytes().await.map_err(|e| CliError::Connection {
            message: e.to_string(),
        })?;

        if !status.is_success() {
            return Err(classify_http_error(
                status.as_u16(),
                &String::from_utf8_lossy(&bytes),
            ));
        }

        let is_json = content_type
            .as_deref()
            .is_some_and(|ct| ct.starts_with("application/json"));
        if is_json {
            let value = serde_json::from_slice(&bytes).map_err(|e| CliError::Server {
                message: format!("invalid JSON in export job response: {e}"),
            })?;
            Ok(ExportResponse::Job(value))
        } else {
            Ok(ExportResponse::File {
                bytes: bytes.to_vec(),
                filename,
            })
        }
    }

    /// `GET /schemas/{schema}/exports/{job_id}` — poll one async export job's
    /// status. Returns the status JSON (`status`, optional `download_url`, etc.).
    pub async fn get_export_job(
        &self,
        schema: &str,
        job_id: &str,
    ) -> Result<Value, CliError> {
        let url = self.url(&["schemas", schema, "exports", job_id])?;
        self.send(Method::GET, url, &[], None)
            .await
            .map(|v| v.unwrap_or(Value::Null))
    }

    /// Download an artifact from a presigned URL.
    ///
    /// The URL is absolute and self-authorizing (a TTL-bounded presigned GET),
    /// so no Bearer token is attached — it points straight at the object store,
    /// not at this instance's API.
    pub async fn download(&self, url: &str) -> Result<Vec<u8>, CliError> {
        let parsed = Url::parse(url).map_err(|e| CliError::Server {
            message: format!("invalid download URL '{url}': {e}"),
        })?;
        let resp = self
            .http
            .get(parsed)
            .send()
            .await
            .map_err(|e| CliError::Connection {
                message: e.to_string(),
            })?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(|e| CliError::Connection {
            message: e.to_string(),
        })?;
        if !status.is_success() {
            return Err(classify_http_error(
                status.as_u16(),
                &String::from_utf8_lossy(&bytes),
            ));
        }
        Ok(bytes.to_vec())
    }
}

/// The two shapes a `POST .../entities/export` can return (ADR-0003): an inline
/// streamed file, or an accepted async job whose artifact is fetched later.
pub enum ExportResponse {
    /// A streamed CSV/NDJSON file (the synchronous, under-cap path).
    File {
        /// The serialized file bytes.
        bytes: Vec<u8>,
        /// Filename suggested by the server's `Content-Disposition`, if any.
        filename: Option<String>,
    },
    /// An accepted async job (XLSX, ZIP, over-cap, or `async: true`). Carries the
    /// job-status JSON (`job_id`, `status`, …).
    Job(Value),
}

/// Extract a `filename="..."` value from a `Content-Disposition` header.
///
/// Pure: returns the unquoted filename when the header carries one, `None`
/// otherwise. Only the common `filename="x"` form is handled (the server emits
/// exactly that); RFC 5987 `filename*` encoding is out of scope.
fn content_disposition_filename(header: Option<&str>) -> Option<String> {
    let header = header?;
    let start = header.find("filename=")? + "filename=".len();
    let rest = &header[start..];
    let trimmed = rest.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|s| s.split('"').next())
        .unwrap_or_else(|| trimmed.split(';').next().unwrap_or(trimmed).trim());
    if unquoted.is_empty() {
        None
    } else {
        Some(unquoted.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forge_base_joins_versioned_path() {
        assert_eq!(
            forge_base("https://forge.example.gov", "v1"),
            "https://forge.example.gov/api/v1/forge"
        );
    }

    #[test]
    fn forge_base_trims_trailing_slash() {
        assert_eq!(
            forge_base("http://127.0.0.1:3000/", "v1"),
            "http://127.0.0.1:3000/api/v1/forge"
        );
    }

    #[test]
    fn classify_extracts_kind_and_message() {
        let body = r#"{"error":"entity_not_found","message":"no such entity"}"#;
        let err = classify_http_error(404, body);
        match err {
            CliError::Http {
                status,
                kind,
                message,
            } => {
                assert_eq!(status, 404);
                assert_eq!(kind, "entity_not_found");
                assert!(message.contains("not found (HTTP 404)"));
                assert!(message.contains("no such entity"));
            }
            other => panic!("expected Http, got {other:?}"),
        }
    }

    #[test]
    fn classify_401_appends_login_tip() {
        let body = r#"{"error":"unauthorized","message":"bad token"}"#;
        let err = classify_http_error(401, body);
        let CliError::Http { message, .. } = err else {
            panic!("expected Http");
        };
        assert!(message.contains("schemaforge login"));
        assert!(message.contains("SCHEMAFORGE_TOKEN"));
    }

    #[test]
    fn classify_422_folds_in_details() {
        let body =
            r#"{"error":"validation_failed","message":"2 invalid","details":["name required","age must be > 0"]}"#;
        let err = classify_http_error(422, body);
        let CliError::Http { message, .. } = err else {
            panic!("expected Http");
        };
        assert!(message.contains("validation failed (HTTP 422)"));
        assert!(message.contains("name required"));
        assert!(message.contains("age must be > 0"));
    }

    #[test]
    fn classify_falls_back_to_raw_body() {
        let err = classify_http_error(500, "upstream exploded");
        let CliError::Http {
            kind, message, ..
        } = err
        else {
            panic!("expected Http");
        };
        assert_eq!(kind, "http_error");
        assert!(message.contains("server error (HTTP 500)"));
        assert!(message.contains("upstream exploded"));
    }

    #[test]
    fn classify_empty_body_uses_status() {
        let err = classify_http_error(503, "");
        let CliError::Http { message, .. } = err else {
            panic!("expected Http");
        };
        assert!(message.contains("HTTP 503"));
    }

    #[test]
    fn content_disposition_extracts_quoted_filename() {
        assert_eq!(
            content_disposition_filename(Some("attachment; filename=\"Contact.csv\"")),
            Some("Contact.csv".to_string())
        );
    }

    #[test]
    fn content_disposition_extracts_unquoted_filename() {
        assert_eq!(
            content_disposition_filename(Some("attachment; filename=Contact.ndjson")),
            Some("Contact.ndjson".to_string())
        );
    }

    #[test]
    fn content_disposition_none_when_absent() {
        assert_eq!(content_disposition_filename(None), None);
        assert_eq!(content_disposition_filename(Some("attachment")), None);
    }
}
