# Changelog

All notable changes to SchemaForge are documented here. The format loosely
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The project
is pre-1.0; breaking changes bump the **minor** version per
[SemVer](https://semver.org/#spec-item-4) for the `0.y.z` series.

## [Unreleased]

### Security / Breaking

- `schemaforge serve` no longer auto-seeds the five SchemaForge demo personas
  (alice, bob, charlie, dana, eve — each with the literal password
  `"password"`) when bootstrapping the admin user via `--admin-user` /
  `--admin-password` (or `FORGE_ADMIN_USER` / `FORGE_ADMIN_PASSWORD`). The
  legacy behavior fired automatically whenever the user store was empty before
  bootstrap, leaving downstream AMI/customer deployments with six accounts and
  five default passwords. **Fixes [#53](https://github.com/govcraft/schemaforge/issues/53).**
- Demo persona seeding is now strictly opt-in via the new
  `--seed-demo-users` flag (env: `FORGE_SEED_DEMO_USERS`, default: `false`).
  The bundled `task demo` flow passes the flag explicitly; `task serve` does
  not.
- `schema_forge_acton::shared_auth::bootstrap_demo_users` gained a required
  `seed: bool` parameter. Existing callers in operator code must pass `false`;
  only controlled local-development flows should pass `true`.
- `SchemaForgeExtensionBuilder` gained `with_seed_demo_users(bool)`. The
  default is `false`, matching the new safe-by-construction posture.

### Migration

Operators upgrading from `schema-forge-cli` 0.27.x:

- If you were relying on the implicit demo seed for local development, add
  `--seed-demo-users` (or set `FORGE_SEED_DEMO_USERS=true`) to your serve
  invocation. The `task demo` Taskfile entry has already been updated.
- If your deployment was unintentionally inheriting the demo accounts, remove
  them with `schemaforge user delete <name>` (or your backend's equivalent) and
  rotate any passwords that may have leaked. They were created with the literal
  string `"password"`.

### Version bumps

- `schema-forge-cli`: 0.27.0 → 0.28.0
- `schema-forge-acton`: 0.26.0 → 0.27.0
