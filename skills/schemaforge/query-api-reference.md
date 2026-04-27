# Query API Reference

SchemaForge generates full CRUD endpoints from `.schema` files, but most
real applications need more than simple get-by-id lookups. The query interface
lets you filter, sort, and paginate entities using either query-string parameters
on GET requests or structured JSON bodies on POST requests.

Both approaches produce the same response format and apply the same access
control rules. Use whichever fits your client best: query strings for simple
lookups from a browser or curl, JSON bodies for complex nested logic built
programmatically.

---

## Table of Contents

1. [Endpoints](#1-endpoints)
2. [Response Format](#2-response-format)
3. [Pagination](#3-pagination)
4. [Sorting](#4-sorting)
5. [Filtering with Query Parameters (GET)](#5-filtering-with-query-parameters-get)
6. [Filtering with JSON Body (POST)](#6-filtering-with-json-body-post)
7. [Filter Operators](#7-filter-operators)
8. [Logical Operators](#8-logical-operators)
9. [Type Coercion](#9-type-coercion)
10. [Dotted Field Paths](#10-dotted-field-paths)
11. [Relation Display Resolution](#11-relation-display-resolution)
12. [Access Control](#12-access-control)
13. [Examples](#13-examples)

---

## 1. Endpoints

### List entities (GET)

```
GET /schemas/{schema}/entities
```

Accepts filter, sort, and pagination as query-string parameters.

### Query entities (POST)

```
POST /schemas/{schema}/entities/query
Content-Type: application/json
```

Accepts a JSON body with `filter`, `sort`, `limit`, and `offset` fields.

Both endpoints require read access to the schema and return the same
`ListEntitiesResponse` shape.

---

## 2. Response Format

```json
{
  "entities": [
    {
      "id": "01J...",
      "schema": "Opportunity",
      "fields": {
        "title": "Cloud Infrastructure Modernization",
        "agency": "entity_01knfncwzmf7h89m7eqn905v0q",
        "agency__display": "Acme Corporation",
        "contacts": ["entity_01abc", "entity_01xyz"],
        "contacts__display": ["Alice", "Bob"],
        "stage": "awarded"
      }
    }
  ],
  "count": 1,
  "total_count": 42
}
```

| Field         | Type                  | Description                                              |
|---------------|-----------------------|----------------------------------------------------------|
| `entities`    | array of objects      | Matching entities after pagination                       |
| `count`       | integer               | Number of entities in this page                          |
| `total_count` | integer or null       | Total matching entities before pagination. Computed by default; `null` when the caller opted out via `?count=false` (GET) or `"count": false` (POST body). |

Each entity object contains:

| Field    | Type   | Description                         |
|----------|--------|-------------------------------------|
| `id`     | string | Entity identifier                   |
| `schema` | string | Schema name                         |
| `fields` | object | Field names mapped to their values  |

When the response includes relation fields, the backend also populates a
`<field>__display` sibling for each one, carrying the target entity's
`@display("...")` value. See [§11 Relation Display Resolution](#11-relation-display-resolution).

---

## 3. Pagination

Both endpoints accept `limit` and `offset`.

| Parameter | Type    | Default | Description              |
|-----------|---------|---------|--------------------------|
| `limit`   | integer | none    | Maximum results to return|
| `offset`  | integer | 0       | Number of results to skip|

**GET example:**

```
GET /schemas/Contact/entities?limit=25&offset=50
```

**POST example:**

```json
{
  "limit": 25,
  "offset": 50
}
```

Use `total_count` from the response to calculate page counts:

```
total_pages = ceil(total_count / limit)
current_page = floor(offset / limit) + 1
```

### Opting out of `total_count`

Computing `total_count` runs a second `COUNT(*)` query in parallel with the
main `SELECT`. Callers that don't need a total (infinite scrolls, one-shot
lookups, dashboards that don't show "X of Y" counters) can skip it to halve
the DB work per request:

| Endpoint                                           | How to opt out                      |
|----------------------------------------------------|-------------------------------------|
| `GET /api/v1/forge/schemas/:schema/entities`       | `?count=false`                      |
| `POST /api/v1/forge/schemas/:schema/entities/query`| Body field `"count": false`         |

Falsy values accepted: `false`, `0`, `no`, `off`. Anything else (including
unset) leaves counting on. When opted out, `total_count` is `null` in the
response.

---

## 4. Sorting

### GET syntax

Pass a `sort` query parameter with comma-separated fields. Two styles are
supported and can be mixed:

**Prefix style** (Django-inspired):

| Prefix | Direction  |
|--------|------------|
| `-`    | Descending |
| `+`    | Ascending  |
| none   | Ascending  |

```
?sort=-age,name
```

Sort by `age` descending, then `name` ascending.

**Colon style:**

```
?sort=age:desc,name:asc
```

Same result as above.

### POST syntax

Pass a `sort` array of objects:

```json
{
  "sort": [
    { "field": "age", "order": "desc" },
    { "field": "name", "order": "asc" }
  ]
}
```

| Field   | Type   | Default | Description                    |
|---------|--------|---------|--------------------------------|
| `field` | string | —       | Field name (supports dotted paths) |
| `order` | string | `"asc"` | `"asc"` or `"desc"`           |

---

## 5. Filtering with Query Parameters (GET)

Filters are expressed as query parameters using double-underscore operator
suffixes:

```
?field__operator=value
```

A bare field name without an operator suffix defaults to equality:

```
?name=Alice          # same as ?name__eq=Alice
```

Multiple filter parameters are combined with AND logic:

```
?name__startswith=A&age__gt=25&status=Active
```

This matches entities where name starts with "A" **and** age is greater than
25 **and** status equals "Active".

### Available operator suffixes

| Suffix          | Operator                    | Example                        |
|-----------------|-----------------------------|--------------------------------|
| `__eq` or none  | Equals                      | `?name=Alice`                  |
| `__ne`          | Not equals                  | `?status__ne=Archived`         |
| `__gt`          | Greater than                | `?age__gt=25`                  |
| `__gte`         | Greater than or equal       | `?age__gte=18`                 |
| `__lt`          | Less than                   | `?age__lt=65`                  |
| `__lte`         | Less than or equal          | `?score__lte=100`              |
| `__contains`    | Substring match             | `?name__contains=ice`          |
| `__startswith`  | Prefix match                | `?email__startswith=admin`     |
| `__in`          | Set membership (comma-sep)  | `?status__in=Active,Pending`   |

The `__in` operator accepts comma-separated values. Each value is individually
type-coerced based on the field's schema type.

### Reserved parameter names

The names `limit`, `offset`, and `sort` are reserved for pagination and sorting.
They cannot be used as filter field names.

---

## 6. Filtering with JSON Body (POST)

The POST endpoint accepts a `filter` object in the request body. Filters are
JSON objects tagged by an `"op"` field:

```json
{
  "filter": {
    "op": "gt",
    "field": "age",
    "value": 25
  }
}
```

This format supports the full set of filter operators and can express nested
logical conditions that query-string filters cannot.

### Comparison operators

```json
{ "op": "eq", "field": "status", "value": "Active" }
{ "op": "ne", "field": "status", "value": "Archived" }
{ "op": "gt", "field": "age", "value": 25 }
{ "op": "gte", "field": "age", "value": 18 }
{ "op": "lt", "field": "age", "value": 65 }
{ "op": "lte", "field": "score", "value": 100 }
```

### String operators

```json
{ "op": "contains", "field": "name", "value": "ice" }
{ "op": "startswith", "field": "email", "value": "admin" }
```

### Set membership

```json
{ "op": "in", "field": "status", "values": ["Active", "Pending"] }
```

### Logical operators

```json
{
  "op": "and",
  "filters": [
    { "op": "gte", "field": "age", "value": 18 },
    { "op": "lt", "field": "age", "value": 65 }
  ]
}
```

```json
{
  "op": "or",
  "filters": [
    { "op": "eq", "field": "status", "value": "Active" },
    { "op": "eq", "field": "status", "value": "Pending" }
  ]
}
```

```json
{
  "op": "not",
  "filter": { "op": "eq", "field": "status", "value": "Archived" }
}
```

Logical operators nest arbitrarily, so you can express any boolean combination.

---

## 7. Filter Operators

| Operator     | Description                        | Value type         |
|--------------|------------------------------------|--------------------|
| `eq`         | Exact equality                     | any                |
| `ne`         | Not equal                          | any                |
| `gt`         | Greater than                       | numeric, datetime  |
| `gte`        | Greater than or equal              | numeric, datetime  |
| `lt`         | Less than                          | numeric, datetime  |
| `lte`        | Less than or equal                 | numeric, datetime  |
| `contains`   | Substring match (case-sensitive)   | string             |
| `startswith` | Prefix match (case-sensitive)      | string             |
| `in`         | Value is in the provided set       | array of any       |

---

## 8. Logical Operators

| Operator | Description                       | Structure                          |
|----------|-----------------------------------|------------------------------------|
| `and`    | All sub-filters must match        | `{ "op": "and", "filters": [...] }`|
| `or`     | At least one sub-filter must match| `{ "op": "or", "filters": [...] }` |
| `not`    | Sub-filter must not match         | `{ "op": "not", "filter": {...} }` |

Note the difference: `and` and `or` take a `"filters"` array, while `not` takes
a single `"filter"` object.

These are only available through the POST JSON body. GET query-string filters
are always AND-combined.

---

## 9. Type Coercion

Filter values are automatically coerced based on the field's type as defined
in the schema:

| Schema field type | Coercion behavior                                      |
|-------------------|--------------------------------------------------------|
| `Integer`         | Parsed as 64-bit integer                               |
| `Float`           | Parsed as 64-bit float                                 |
| `Boolean`         | `"true"` / `"1"` → true, `"false"` / `"0"` → false   |
| `DateTime`        | Parsed as ISO 8601 UTC (e.g. `2024-01-15T09:30:00Z`)  |
| `Enum`            | Accepted as string, validated against schema variants  |
| `Text` / `RichText` | Kept as string                                      |

For GET requests, all values arrive as strings and are coerced using these
rules. For POST requests, JSON native types (numbers, booleans) are used
directly, with string values coerced when the schema type requires it.

Invalid coercions return a 400 error with a description of the type mismatch.

---

## 10. Dotted Field Paths

Field names support dotted notation for traversing relations:

```
?sort=-company.name
```

```json
{ "op": "eq", "field": "company.industry", "value": "Technology" }
```

This lets you filter or sort by fields on related entities. The path follows
the relation chain defined in your schema: `company.industry` means "the
`industry` field on the entity referenced by the `company` relation".

---

## 11. Relation Display Resolution

Generated sites rarely want to render relation fields as raw entity IDs
(`entity_01knfncwzmf7h89m7eqn905v0q`). SchemaForge does the resolution
server-side: for every relation field on the result schema, the backend
issues a single batched `IN`-query against the target schema and joins the
target's `@display("field")` value back onto each row.

### Envelope shape

Each relation field gains a sibling `<field>__display` key on `fields`:

| Relation kind   | Sibling type                  | Description                                              |
|-----------------|-------------------------------|----------------------------------------------------------|
| `relation_one`  | `string`                      | The resolved display value for the referenced entity.     |
| `relation_many` | `(string \| null)[]`          | Parallel array in declaration order; `null` for unresolvable slots. |

Example payload:

```json
{
  "id": "entity_01opp...",
  "schema": "Opportunity",
  "fields": {
    "title": "Cloud Infrastructure Modernization",
    "agency": "entity_01knfncwzmf7h89m7eqn905v0q",
    "agency__display": "Acme Corporation",
    "contacts": ["entity_01abc", "entity_01xyz"],
    "contacts__display": ["Alice Smith", null]
  }
}
```

The `null` in `contacts__display[1]` means the second contact could not be
resolved (the target entity was deleted, the target schema has no
`@display`, or the ID doesn't exist under the current tenant scope).

### Default-on with opt-out

Resolution is **on by default** so the generated site gets human-readable
labels without extra client-side fetches. Callers that don't need them
(API-only consumers, bulk exports) can opt out to skip the batched query:

| Endpoint                                           | How to opt out                      |
|----------------------------------------------------|-------------------------------------|
| `GET /api/v1/forge/schemas/:schema/entities`       | `?resolve=false`                    |
| `GET /api/v1/forge/schemas/:schema/entities/:id`   | `?resolve=false`                    |
| `POST /api/v1/forge/schemas/:schema/entities/query`| Body field `"resolve": false`       |

Falsy values accepted: `false`, `0`, `no`, `off`. Anything else (including
unset) leaves resolution on.

### Cost model

- **One extra query per relation target schema per list call**, regardless
  of row count. A 50-row Opportunity list with five relation columns
  pointing at four distinct target schemas costs four IN-queries total.
- **Constant-time at the row level.** The IN-query projection is narrowed
  to `id` and the display field only, so resolution never reads unrelated
  columns.
- **Tenant-scoped.** The resolution query runs through the same tenant
  scope filter as the primary query, so callers can never observe display
  values for entities they couldn't otherwise see.
- **Missing `@display` gracefully ignored.** Target schemas without an
  `@display("...")` annotation are skipped — the envelope simply omits
  their sibling keys and clients fall back to the raw ID.

### Generator support

The site generator emits typed sibling fields on the generated
`entity-types.ts` so TypeScript clients see them without any extra
wiring:

```ts
export interface Opportunity {
  id: string
  title: string
  agency?: string
  /** Resolved display value for `agency` (Agency.name). */
  agency__display?: string
  contacts?: string[]
  /** Resolved display values for `contacts` (Contact.full_name). */
  contacts__display?: (string | null)[]
}
```

Both `list.tsx` and `detail.tsx` templates prefer the `__display` value
over the raw ID when rendering relation cells, falling back to the ID if
resolution is missing.

## 12. Access Control

Query results are filtered through multiple access control layers:

1. **Schema-level access** — the caller must have read permission on the schema.
   Unauthenticated requests are rejected if the schema requires authentication.

2. **Tenant scope** — in multi-tenant configurations, a tenant filter is
   automatically injected into every query. Callers only see entities belonging
   to their tenant.

3. **Record-level visibility** — schemas with `@owner` or similar annotations
   restrict which records are visible to each caller. Applied after the query
   executes.

4. **Field-level filtering** — fields with read restrictions are stripped from
   the response. The entity still appears, but restricted fields are omitted.

All of this happens transparently. You do not need to add tenant or ownership
filters to your queries manually.

---

## 13. Examples

### Simple list with pagination

```bash
curl 'http://localhost:3000/schemas/Contact/entities?limit=10&offset=0'
```

### Filter by status and sort by name

```bash
curl 'http://localhost:3000/schemas/Contact/entities?status=Active&sort=name'
```

### Age range with descending sort

```bash
curl 'http://localhost:3000/schemas/Contact/entities?age__gte=18&age__lt=65&sort=-age'
```

### Set membership

```bash
curl 'http://localhost:3000/schemas/Contact/entities?status__in=Active,Pending&limit=50'
```

### Substring search

```bash
curl 'http://localhost:3000/schemas/Contact/entities?name__contains=smith&sort=name'
```

### Complex query with POST

```bash
curl -X POST 'http://localhost:3000/schemas/Contact/entities/query' \
  -H 'Content-Type: application/json' \
  -d '{
    "filter": {
      "op": "and",
      "filters": [
        {
          "op": "or",
          "filters": [
            { "op": "eq", "field": "status", "value": "Active" },
            { "op": "eq", "field": "status", "value": "Pending" }
          ]
        },
        { "op": "gte", "field": "age", "value": 18 },
        { "op": "startswith", "field": "name", "value": "A" }
      ]
    },
    "sort": [
      { "field": "name", "order": "asc" }
    ],
    "limit": 25,
    "offset": 0
  }'
```

### Negated filter

```bash
curl -X POST 'http://localhost:3000/schemas/Contact/entities/query' \
  -H 'Content-Type: application/json' \
  -d '{
    "filter": {
      "op": "not",
      "filter": { "op": "eq", "field": "status", "value": "Archived" }
    }
  }'
```

### Sort by related field

```bash
curl 'http://localhost:3000/schemas/Contact/entities?sort=-company.name&limit=20'
```
