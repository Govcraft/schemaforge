# Generated field types and authority, issues 191 and 192

Use the existing view-model and template architecture. Keep display types complete while filtering form authority separately.

- Add duration (Go-style duration string validation), bytes (base64 text with decoded size constraint), and map (typed Record JSON textarea) mapping. Preserve recursive composite/array type projection. Format duration wire strings into readable units without altering submitted values.
- Project computed/read-role/write-role metadata on FieldView. Omit computed and derived fields from form controls and form validators. Use current auth roles at render/parse/submit time, not module initialization. Hide read-denied controls, render write-denied values inert, and ensure validation does not require denied fields.
- Use recursive metadata to filter initial and submitted state, stripping read-denied, computed/derived and unwritable payload fields including nested composites. Preserve readable, write-denied initial values for read-only display. Normalize JSON and composite values recursively so nested supported fields remain round-trippable.
- Update field-type and permissions documentation. Add generator regression checks plus Playwright behavior tests for serialization, validation and role changes. Fail generation for any remaining unsupported required field.
- Preserve existing generated styling and accessibility labels, avoiding controls which imply unavailable actions (UI design expert, interaction patterns).

No new dependencies or error types required. Semver: fixes in release already planned by root. Run only targeted cargo check locally; root runs generation, TypeScript build/lint, and browser checks in CI. Sign conventional commits; no push.
