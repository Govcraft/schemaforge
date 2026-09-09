# Invite users from a generated site

Generated React sites include an **Invite user** navigation entry when the authenticated caller can create Users. Open `/admin/users/invite`, enter the recipient's email, and optionally select a display name, role, and tenant. Roles come from the deployment's live role catalog. Tenant choices include only records whose server-computed update permission is true. The backend independently validates every invitation and role grant.

The recipient opens `/invite/accept?invite=<reference>` from their email and chooses a password. This route works without signing in, including when another account already has a session in the browser. Acceptance never sends that session's credentials. Successful acceptance opens the login page. Missing, expired, consumed, conflicting, and failed invitations show visible errors without redirecting away from the form.

Email transport must be configured on the backend. A server error during email delivery is shown as a failure; the site does not expose a development bypass or claim the invitation was delivered.

The former dynamic admin console moved to a separate repository. These routes belong to this repository's current `site generate` output. Invitation page layouts are preserved on regeneration, while API helpers and route metadata are regenerated. Existing generated sites receive the new routes through their regenerated shell and helpers.

Fields annotated `@hidden` are excluded from generated entity views and form validation, including composite sub-fields. Required hidden fields therefore do not create an impossible client-side validation requirement. Backend field authorization remains authoritative.
