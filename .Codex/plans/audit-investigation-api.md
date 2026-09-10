# Audit investigation API and capture

Add a backward-compatible investigation mode to the administrator-only audit API. Preserve legacy ascending snapshot reads. Newest-first browsing accepts exact allowlisted filters and exclusive sequence cursors bounded by a fixed inclusive snapshot. Use acton-service AuditQuery and query_filtered; do not scan or introduce storage infrastructure.

Projection is a pure AuditEvent -> EventView conversion. Retain existing integrity fields and add source identity/request correlation, request method and query-free path, status and duration. Only known forge event kinds may expose explicit schema/entity/user/actor/target/tenant/changed-field/reason keys. Never expose arbitrary metadata or values. Optional strings retain existing identifier wire formats.

Modify routes/audit.rs (query DTO, validation, projection, bounded query), routes/entities.rs (safe field-name and request context capture), routes/auth.rs (request correlation), and existing audit integration tests. Reuse ForgeError for API errors. Preserve platform_admin authorization before query parsing. Unknown filters, invalid date ranges, unsupported order and invalid cursor combinations must fail.

Verify projection redaction, unknown event suppression, authorization, descending pagination with filters and stable snapshot, validation, request context capture. Run cargo nextest and clippy with warnings denied. Additive minor bump for affected acton and CLI crates after reviewing release history. Dependency changes use cargo add when released; temporary local patch only for coordinated validation.

Review additions: project only typed role before/after arrays from user-update events, activation boolean from active-toggle events, and self-service boolean from password-change events. Preserve null versus empty roles. Reuse the password handler's existing self-authorization result for its audit flag and resolve its request source. Redact connection userinfo and URL options in CLI startup/failure messages and Display, using the existing URL parser and fully redacting non-URL DSNs. Add regression coverage and re-run relevant tests and clippy before committing.
