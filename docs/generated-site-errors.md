# Generated site request context and error handling

The generated API client sends the current `X-Active-Tenant` selection on entity and invitation requests, including retries after token refresh. File upload and download API calls use the same session header builder. An explicit tenant header in a request takes precedence, regardless of header casing or whether headers are supplied as an object, tuples, or a `Headers` instance. Public login and invitation acceptance do not require a tenant selection.

`ApiError` retains `status`, parsed `body`, and `raw` for application logic and diagnostics. Its message and `formatApiError(error)` display the human message from either supported API envelope, with status and network fallbacks when needed. Components should use the formatter rather than displaying the raw response.

Global query and mutation notifications live in `src/lib/error-toast.ts`. The generator scaffolds this file once and preserves your changes during regeneration and `--check`. Customize its exported handlers to change notification appearance, send errors to monitoring, or disable global notifications entirely.

Queries and mutations can set `meta: { suppressGlobalError: true }` when they render their own recovery controls. The default global mutation handler also skips mutations with their own `onError` callback. Generated edit forms handle unique conflicts inline and show one local notification for other save failures. Generated list and detail pages render an inline error and Retry button.

When upgrading an existing generated project, regenerate the owned client and shared components to receive the fixes. Your existing page shells are preserved. Add `meta: { suppressGlobalError: true }` to list and detail queries that already render `ErrorBlock`, and use `formatApiError(error)` in custom local handlers. Existing edit mutations with `onError` automatically avoid duplicate global notifications through the new default global handler. If you previously customized `src/lib/error-toast.ts`, retain or adapt those policies explicitly.
