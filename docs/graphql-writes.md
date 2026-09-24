# GraphQL entity writes

GraphQL `create{Schema}` and `update{Schema}` mutations use the same entity write pipeline as REST POST and PATCH. Field authorization runs before defaults, computed expressions, validation rules, and lifecycle hooks. Updates merge authorized input with stored fields before evaluating rules. A mutation containing only denied fields returns the unchanged entity, unless server-generated values change.

Required fields supplied by a literal default, `@default`, `@compute`, or `@owner` may be omitted from create input. Required fields without a server-supplied value remain mandatory. The completed entity is validated before persistence.

Mutation responses use the same field projection as REST, including hidden-field removal and field-level read authorization. A GraphQL update has partial-update semantics; it does not behave like REST PUT.

## Rust integration migration

`SchemaForgeExtension::register_graphql_routes` now accepts and returns `Router<AppState<SchemaForgeConfig>>`. Register GraphQL routes on the service router before supplying the initialized application state. The service must register and initialize `ForgeActor`, as required by the REST routes; register `HookDispatchActor` when lifecycle hooks are enabled. GraphQL no longer writes directly through a separate backend-only state.

The request context carries both the schema/query state and the initialized actor-backed application state. Embedders constructing `ForgeGraphqlContext` directly must supply its new `app_state` field.
