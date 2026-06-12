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

use std::collections::BTreeMap;
use std::path::Path;

use reqwest::{Body, Client, Method, Url};
use serde::Deserialize;
use serde_json::Value;
use tokio::fs::File;
use tokio_util::io::ReaderStream;

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

/// Client-side view of the server's `MintUploadUrlResponse`.
///
/// The `headers` map MUST be replayed verbatim on the subsequent PUT: the
/// presigned signature binds them, so dropping or altering one yields a 403
/// `SignatureDoesNotMatch` from S3.
#[derive(Debug, Deserialize)]
pub struct UploadUrlDto {
    /// Fully-qualified URL the client PUTs bytes to.
    pub upload_url: String,
    /// Server-generated object key, echoed back on confirm.
    pub key: String,
    /// Headers the client MUST include on the PUT request, verbatim.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

/// Result of a successful file download.
#[derive(Debug)]
pub struct DownloadOutcome {
    /// Total bytes written to the sink.
    pub bytes_written: u64,
    /// The final response URL after any redirects, used to derive a default
    /// output filename when the caller did not pass `--out`.
    pub final_url: String,
}

/// Trim an opaque (often XML) error body to a single legible line.
///
/// Pure: collapses interior whitespace and caps the length so a multi-kilobyte
/// S3 error page cannot flood the terminal. Empty input yields `"<no body>"`.
fn snippet(body: &str) -> String {
    const MAX: usize = 300;
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return "<no body>".to_string();
    }
    if collapsed.len() > MAX {
        // Cap at MAX bytes, snapping back to a char boundary so a multibyte
        // codepoint straddling the cut never panics on the slice.
        let mut end = MAX;
        while end > 0 && !collapsed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &collapsed[..end])
    } else {
        collapsed
    }
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

    // --- File field operations --------------------------------------------

    /// `POST /schemas/{schema}/entities/{id}/fields/{field}/upload-url`.
    ///
    /// Mints a presigned PUT URL plus the exact headers the client must replay
    /// on the upload. A `before_upload` hook abort or a mime/size mismatch
    /// arrives here as a forge error envelope and is mapped by
    /// [`classify_http_error`].
    pub async fn mint_upload_url(
        &self,
        schema: &str,
        id: &str,
        field: &str,
        filename: &str,
        mime: &str,
        size: u64,
    ) -> Result<UploadUrlDto, CliError> {
        let url = self.url(&[
            "schemas", schema, "entities", id, "fields", field, "upload-url",
        ])?;
        let body = serde_json::json!({
            "filename": filename,
            "mime": mime,
            "size": size,
        });
        let value = self
            .send(Method::POST, url, &[], Some(&body))
            .await?
            .unwrap_or(Value::Null);
        serde_json::from_value(value).map_err(|e| CliError::Server {
            message: format!("unexpected upload-url response: {e}"),
        })
    }

    /// PUT the file's bytes to the presigned `upload_url`, streamed from disk.
    ///
    /// Unauthenticated by design: the presigned URL carries its own auth, so
    /// no Bearer token is attached. Every header from the mint response is
    /// replayed verbatim. The response comes from S3, not the forge server, so
    /// a non-2xx is rendered with the status and a short XML body snippet
    /// rather than run through the forge-envelope parser.
    pub async fn put_file_bytes(
        &self,
        upload_url: &str,
        headers: &BTreeMap<String, String>,
        path: &Path,
        size: u64,
    ) -> Result<(), CliError> {
        let file = File::open(path).await.map_err(|e| CliError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        let body = Body::wrap_stream(ReaderStream::new(file));

        let mut req = self
            .http
            .put(upload_url)
            .header(reqwest::header::CONTENT_LENGTH, size)
            .body(body);
        for (name, value) in headers {
            req = req.header(name, value);
        }

        let resp = req.send().await.map_err(|e| CliError::Connection {
            message: e.to_string(),
        })?;

        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        // S3 errors are XML, not the forge JSON envelope. Surface the status
        // and a trimmed snippet so the signature/permission cause is legible.
        let snippet = resp
            .text()
            .await
            .map(|t| snippet(&t))
            .unwrap_or_default();
        Err(CliError::Connection {
            message: format!("upload to storage failed (HTTP {status}): {snippet}"),
        })
    }

    /// `POST /schemas/{schema}/entities/{id}/fields/{field}/confirm-upload`.
    ///
    /// Hands the object key (and optional SHA-256) back to the runtime, which
    /// HEADs the object, re-validates size/mime, and persists the attachment.
    /// Returns the `{ status, attachment }` envelope.
    pub async fn confirm_upload(
        &self,
        schema: &str,
        id: &str,
        field: &str,
        key: &str,
        checksum_sha256: Option<&str>,
    ) -> Result<Value, CliError> {
        let url = self.url(&[
            "schemas", schema, "entities", id, "fields", field, "confirm-upload",
        ])?;
        let mut body = serde_json::Map::new();
        body.insert("key".to_string(), Value::String(key.to_string()));
        if let Some(sum) = checksum_sha256 {
            body.insert(
                "checksum_sha256".to_string(),
                Value::String(sum.to_string()),
            );
        }
        self.send(Method::POST, url, &[], Some(&Value::Object(body)))
            .await
            .map(|v| v.unwrap_or(Value::Null))
    }

    /// `GET /schemas/{schema}/entities/{id}/fields/{field}` — download.
    ///
    /// Sends the Bearer token and follows redirects (reqwest's default policy
    /// strips `Authorization` on cross-host hops, which is exactly right for
    /// the presigned-mode 302 to S3). The streamed body is written chunk by
    /// chunk to `sink`; the final response URL is returned so the caller can
    /// derive a default filename. A non-available attachment is a forge error
    /// envelope mapped by [`classify_http_error`].
    pub async fn download_file<W>(
        &self,
        schema: &str,
        id: &str,
        field: &str,
        sink: &mut W,
    ) -> Result<DownloadOutcome, CliError>
    where
        W: tokio::io::AsyncWrite + Unpin,
    {
        use tokio::io::AsyncWriteExt;

        let url = self.url(&["schemas", schema, "entities", id, "fields", field])?;
        let mut req = self.http.get(url);
        if let Some(tok) = &self.token {
            req = req.bearer_auth(tok);
        }
        let mut resp = req.send().await.map_err(|e| CliError::Connection {
            message: e.to_string(),
        })?;

        let status = resp.status();
        let final_url = resp.url().to_string();
        if !status.is_success() {
            let text = resp.text().await.map_err(|e| CliError::Connection {
                message: e.to_string(),
            })?;
            return Err(classify_http_error(status.as_u16(), &text));
        }

        let mut written: u64 = 0;
        while let Some(chunk) = resp.chunk().await.map_err(|e| CliError::Connection {
            message: e.to_string(),
        })? {
            sink.write_all(&chunk).await.map_err(|e| CliError::Io {
                path: std::path::PathBuf::from("<download>"),
                source: e,
            })?;
            written += chunk.len() as u64;
        }
        sink.flush().await.map_err(|e| CliError::Io {
            path: std::path::PathBuf::from("<download>"),
            source: e,
        })?;

        Ok(DownloadOutcome {
            bytes_written: written,
            final_url,
        })
    }

    /// `POST /auth/login` — exchange credentials for a token. Unauthenticated.
    pub async fn login(&self, username: &str, password: &str) -> Result<Value, CliError> {
        let url = self.url(&["auth", "login"])?;
        let body = serde_json::json!({ "username": username, "password": password });
        self.send(Method::POST, url, &[], Some(&body))
            .await
            .map(|v| v.unwrap_or(Value::Null))
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
    fn snippet_collapses_whitespace_and_caps_length() {
        assert_eq!(snippet(""), "<no body>");
        assert_eq!(snippet("   \n\t  "), "<no body>");
        assert_eq!(
            snippet("<Error>\n  <Code>SignatureDoesNotMatch</Code>\n</Error>"),
            "<Error> <Code>SignatureDoesNotMatch</Code> </Error>"
        );
        let long = "x".repeat(500);
        let out = snippet(&long);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().filter(|&c| c == 'x').count(), 300);
    }

    #[test]
    fn snippet_caps_on_char_boundary_without_panicking() {
        // A 3-byte codepoint repeated past the cap: truncating at byte 300
        // would split a codepoint, so the cut must snap to a boundary.
        let multibyte = "あ".repeat(200); // 600 bytes, 200 chars
        let out = snippet(&multibyte);
        assert!(out.ends_with('…'));
        // 300-byte budget / 3 bytes per char = 100 whole chars before the ellipsis.
        assert_eq!(out.chars().filter(|&c| c == 'あ').count(), 100);
    }
}
