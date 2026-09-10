# Custom policies and authorization context

Application actions and field actions receive a required Boolean request context attribute, `context.resource_is_placeholder`. SchemaForge sets it from the resource supplied to the authorization engine. Claims, request bodies, and stored field values cannot override it.

| Engine input | Context value | Resource attributes |
|---|---|---|
| No concrete entity, including a schema preflight | `true` | Synthetic defaults for required representable fields; optional fields absent |
| A concrete entity supplied to the engine | `false` | That entity's represented fields |
| A field read or write check | `false` | The concrete entity supplied for field authorization |

A schema preflight uses a Cedar resource UID such as `Notice::"_any"`. It supplies defaults because strict entity validation requires declared required attributes: empty strings for text, enum, file and single relations; zero for integer, float and datetime; false for Boolean; empty sets for arrays and multi-relations. Hidden and unrepresented fields remain absent. These values are not proposed or stored data. A concrete record can contain the same values and still has `resource_is_placeholder = false`.

Read preflights occur for both collection requests and point GET requests. List routes currently check the `ReadNotice` action at schema scope before checking each concrete row with that same action. Checking only `ListNotice` does not cover that route's preflight. Permission discovery and other schema checks also use placeholders; do not infer the scope solely from the action name.

## Restrict readable records without blocking their preflight

Suppose a role already receives schema read permission and only records with `visible = true` should be readable. Scope the restriction to the **Read** action and concrete resources:

```cedar
forbid (
    principal,
    action == Action::"ReadNotice",
    resource is Notice
)
when {
    !context.resource_is_placeholder &&
    !(resource has visible && resource.visible)
};
```

The preflight still needs a permit from an applicable policy. Concrete false values and absent optional values are denied. The `has` guard is required for optional fields and remains valid when the field is required. Omitting the context guard preserves the previous behavior: a required false placeholder can deny the entire preflight. SchemaForge does not rewrite or bypass existing custom forbids.

## Admit preflight for a conditional Read permit

A role whose only Read permission comes from a conditional custom permit must also be admitted at schema scope:

```cedar
permit (
    principal in Forge::Group::"guest_reviewer",
    action == Action::"ReadNotice",
    resource is Notice
)
when {
    context.resource_is_placeholder ||
    (resource has visible && resource.visible)
};
```

The role restriction applies to both branches. This grants a preflight and readable concrete records to that role. Cedar permits are additive: this condition does not narrow a broader generated or custom permit. Use an appropriately scoped forbid when a restriction must override other permits.

## Create and other action boundaries

The generic entity Create route currently checks schema access without passing the proposed entity to `authorize`. That check sees a placeholder. A Create forbid guarded by `!context.resource_is_placeholder` would therefore not enforce a condition on proposed fields. Do not copy the Read example to Create expecting record validation. Use applicable schema constraints, `@require` rules, or before-change hooks to validate proposed values while retaining the intended schema authorization gate.

Update and delete routes perform schema checks and concrete-resource checks. Field checks always receive an entity. The context flag describes only whether the individual engine call has a placeholder; it does not promise that the entity is persisted, contains proposed changes, or that another check will run later. Review the relevant route before assigning a policy to a different action.

## Diagnostics and manual Cedar callers

Decision logs include `resource_is_placeholder` and the actual `resource_uid`. A placeholder denial reports `resource_is_placeholder=true` and a UID ending in `::"_any"`; a concrete denial reports false and the record UID. Denials include `matched_policies` and evaluation `errors`. Evaluation errors fail closed and are logged as denials even if Cedar also found a matching permit. These fields describe the evaluation, not the correctness of the operator's policy.

The generated application and field action schema now requires this Boolean context attribute. Framework entry points supply it automatically. Code constructing Cedar requests directly against the generated schema must provide it; an empty context fails strict request validation. Set the flag according to whether the request actually uses a placeholder, and derive it in trusted server code. Schema-administration actions are outside this application-action context contract.

This changes the generated Cedar request contract for manual consumers and must be considered in release compatibility and semantic version review. It does not add a new HTTP request field, weaken strict entity validation, or automatically make existing attribute-dependent policies safe for preflights. Validate custom policies against the generated schema and test both placeholder and concrete evaluations.
