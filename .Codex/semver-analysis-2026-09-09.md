# SchemaForge release semver analysis

Analyzed v0.43.0 through fa080a2, including issues #151, #152, and #155. Recommend CLI 0.44.0 and independently versioned schema-forge-acton 0.43.0. Other package versions remain unchanged.

The generated Cedar request schema now requires Boolean context.resource_is_placeholder. Manual callers supplying empty context fail strict validation. The owner restriction no longer denies reads, widening access for roles with schema read permission. These are compatibility changes requiring the next minor boundary under the established pre-1.0 policy.

Authorized pagination is a correction to the intended API contract. Exact counts may increase query cost and now reflect readable records. acton-service 0.43.1 is a compatible framework patch for generated request correlation; the release resolves it from crates.io.

No public Rust signature, struct, enum, trait, feature, edition, or compiler requirement changed in the application fixes. The important migrations concern security behavior and generated policy requests. Framework callers receive context automatically; custom policies are not rewritten. Read guards must not be used as proposed-field Create validation.

Release notes are based on the actual tag-to-main delta. Older unversioned changelog entries were already delivered and remain separated as historical notes.
