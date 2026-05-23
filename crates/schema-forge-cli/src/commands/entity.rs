//! `schemaforge entity <verb>` — ergonomic CRUD against a running instance.
//!
//! This group speaks the entity REST API over HTTP (see [`crate::http`]),
//! authenticating with a Bearer token. It is intentionally *not* a curl
//! clone: field input is typed (`--set name=Alice`, `field:=json`), filters
//! mirror the server's `field__op=value` grammar, and output reuses the
//! shared [`OutputContext`] (table for lists, key/value for a single entity,
//! raw JSON under `--format json`).

use console::Term;
use serde_json::{json, Map, Value};

use crate::cli::{
    EntityCommands, EntityConnectionArgs, EntityCreateArgs, EntityDeleteArgs, EntityGetArgs,
    EntityInputArgs, EntityListArgs, EntityQueryArgs, EntityWriteArgs, GlobalOpts,
};
use crate::config::{load_svc_config, resolve_client_config, ResolvedClient};
use crate::error::CliError;
use crate::http::{forge_base, EntityDto, ForgeClient, ListDto};
use crate::output::{OutputContext, OutputMode};
use crate::progress;

/// Dispatch an `entity` subcommand.
pub async fn run(
    command: EntityCommands,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    match command {
        EntityCommands::List(args) => list(*args, global, output).await,
        EntityCommands::Get(args) => get(*args, global, output).await,
        EntityCommands::Create(args) => create(*args, global, output).await,
        EntityCommands::Replace(args) => write(*args, global, output, true).await,
        EntityCommands::Patch(args) => write(*args, global, output, false).await,
        EntityCommands::Delete(args) => delete(*args, global, output).await,
        EntityCommands::Query(args) => query(*args, global, output).await,
    }
}

// ---------------------------------------------------------------------------
// Verb handlers
// ---------------------------------------------------------------------------

async fn list(
    args: EntityListArgs,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let query = list_query_params(&args)?;
    let (_, client) = build_client(&args.conn, global, output, true)?;
    let value = call_with_spinner(output, "Listing entities…", client.list(&args.schema, &query)).await?;
    render_list(output, &value, args.fields.as_deref())
}

async fn get(
    args: EntityGetArgs,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let mut q: Vec<(String, String)> = Vec::new();
    if let Some(f) = &args.fields {
        q.push(("fields".to_string(), f.clone()));
    }
    if args.no_resolve {
        q.push(("resolve".to_string(), "false".to_string()));
    }
    let (_, client) = build_client(&args.conn, global, output, true)?;
    let value =
        call_with_spinner(output, "Fetching entity…", client.get(&args.schema, &args.id, &q)).await?;
    render_entity(output, &value)
}

async fn create(
    args: EntityCreateArgs,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let body = resolve_body(&args.input)?;
    let (rc, client) = build_client(&args.conn, global, output, !args.dry_run)?;
    let path = format!("/schemas/{}/entities", args.schema);
    if args.dry_run {
        return dry_run_report(output, &rc, "POST", &path, Some(&body));
    }
    let value =
        call_with_spinner(output, "Creating entity…", client.create(&args.schema, &body)).await?;
    report_write(output, "created", &value)
}

async fn write(
    args: EntityWriteArgs,
    global: &GlobalOpts,
    output: &OutputContext,
    replace: bool,
) -> Result<(), CliError> {
    let body = resolve_body(&args.input)?;
    let (rc, client) = build_client(&args.conn, global, output, !args.dry_run)?;
    let method = if replace { "PUT" } else { "PATCH" };
    let path = format!("/schemas/{}/entities/{}", args.schema, args.id);
    if args.dry_run {
        return dry_run_report(output, &rc, method, &path, Some(&body));
    }
    let value = if replace {
        call_with_spinner(
            output,
            "Replacing entity…",
            client.replace(&args.schema, &args.id, &body),
        )
        .await?
    } else {
        call_with_spinner(
            output,
            "Patching entity…",
            client.patch(&args.schema, &args.id, &body),
        )
        .await?
    };
    report_write(output, if replace { "replaced" } else { "patched" }, &value)
}

async fn delete(
    args: EntityDeleteArgs,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let (rc, client) = build_client(&args.conn, global, output, !args.dry_run)?;
    let path = format!("/schemas/{}/entities/{}", args.schema, args.id);
    if args.dry_run {
        return dry_run_report(output, &rc, "DELETE", &path, None);
    }

    if !args.yes {
        // Remote + destructive: confirm on a TTY, refuse in non-interactive
        // contexts (mirrors the `apply`/`migrate` safety pattern).
        if !Term::stderr().is_term() {
            return Err(CliError::RequiresForce);
        }
        let confirmed = dialoguer::Confirm::new()
            .with_prompt(format!("Delete {}/{}?", args.schema, args.id))
            .default(false)
            .interact()
            .map_err(|_| CliError::Cancelled)?;
        if !confirmed {
            return Err(CliError::Cancelled);
        }
    }

    call_with_spinner(
        output,
        "Deleting entity…",
        client.delete(&args.schema, &args.id),
    )
    .await?;
    output.success(&format!("deleted {}/{}", args.schema, args.id));
    Ok(())
}

async fn query(
    args: EntityQueryArgs,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let filter_str = match &args.filter {
        Some(arg) => Some(read_data_arg(arg)?),
        None => None,
    };
    let body = build_query_body(
        filter_str.as_deref(),
        args.sort.as_deref(),
        args.fields.as_deref(),
        args.limit,
        args.offset,
        args.no_count,
        args.no_resolve,
    )?;
    let (_, client) = build_client(&args.conn, global, output, true)?;
    let value =
        call_with_spinner(output, "Querying entities…", client.query(&args.schema, &body)).await?;
    render_list(output, &value, args.fields.as_deref())
}

// ---------------------------------------------------------------------------
// Client construction
// ---------------------------------------------------------------------------

/// Resolve connection settings and build the HTTP client.
///
/// When `require_token` is set (every real call except `--dry-run`), a
/// missing token is a hard error with a pointer to the auth flow rather than
/// a later, more cryptic 401.
fn build_client(
    conn: &EntityConnectionArgs,
    global: &GlobalOpts,
    output: &OutputContext,
    require_token: bool,
) -> Result<(ResolvedClient, ForgeClient), CliError> {
    let svc = load_svc_config(global)?;
    let rc = resolve_client_config(&svc, conn)?;
    if require_token && rc.token.is_none() {
        return Err(CliError::Config {
            message: "no API token found. Run `schemaforge login`, or provide \
                      --token-file / --token-stdin / set SCHEMAFORGE_TOKEN."
                .to_string(),
        });
    }
    let client = ForgeClient::new(&rc, output)?;
    Ok((rc, client))
}

/// Run a network call under a spinner (cleared on success, shown on error).
async fn call_with_spinner<T>(
    output: &OutputContext,
    msg: &str,
    fut: impl std::future::Future<Output = Result<T, CliError>>,
) -> Result<T, CliError> {
    let spinner = output.show_progress().then(|| progress::create_spinner(msg));
    let result = fut.await;
    if let Some(sp) = spinner {
        match &result {
            Ok(_) => sp.finish_and_clear(),
            Err(e) => progress::finish_spinner_error(&sp, &e.to_string()),
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Request body / query construction (pure helpers)
// ---------------------------------------------------------------------------

/// Resolve the `{ "fields": ... }` body for create/replace/patch from
/// `--set` and/or `--data`.
fn resolve_body(input: &EntityInputArgs) -> Result<Value, CliError> {
    if input.set.is_empty() && input.data.is_none() {
        return Err(CliError::Config {
            message: "no fields to send: provide --set field=value and/or --data".to_string(),
        });
    }
    let data = match &input.data {
        Some(arg) => Some(read_data_arg(arg)?),
        None => None,
    };
    build_fields_body(&input.set, data.as_deref())
}

/// Read a `--data`/`--filter` argument: `-` (stdin), `@file`, or a literal.
fn read_data_arg(arg: &str) -> Result<String, CliError> {
    use std::io::Read;
    if arg == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| CliError::Io {
                path: std::path::PathBuf::from("<stdin>"),
                source: e,
            })?;
        Ok(buf)
    } else if let Some(path) = arg.strip_prefix('@') {
        std::fs::read_to_string(path).map_err(|e| CliError::Io {
            path: std::path::PathBuf::from(path),
            source: e,
        })
    } else {
        Ok(arg.to_string())
    }
}

/// Build `{ "fields": {...} }` by overlaying `--set` pairs onto an optional
/// `--data` base. `data` may be a bare field map or an already-wrapped
/// `{ "fields": ... }` object; `--set` values win on conflict.
fn build_fields_body(sets: &[String], data: Option<&str>) -> Result<Value, CliError> {
    let mut fields: Map<String, Value> = match data {
        Some(text) => {
            let parsed: Value = serde_json::from_str(text).map_err(|e| CliError::Config {
                message: format!("--data is not valid JSON: {e}"),
            })?;
            let obj = parsed.as_object().ok_or_else(|| CliError::Config {
                message: "--data must be a JSON object".to_string(),
            })?;
            // Accept an already-wrapped { "fields": {...} } envelope.
            match obj.get("fields") {
                Some(Value::Object(inner)) if obj.len() == 1 => inner.clone(),
                _ => obj.clone(),
            }
        }
        None => Map::new(),
    };

    for raw in sets {
        let (key, value) = parse_set_arg(raw).map_err(|message| CliError::Config { message })?;
        fields.insert(key, value);
    }

    Ok(json!({ "fields": Value::Object(fields) }))
}

/// Parse one `--set` argument. `field:=json` carries an explicit JSON value;
/// `field=value` is a typed scalar (see [`coerce_scalar`]).
fn parse_set_arg(raw: &str) -> Result<(String, Value), String> {
    let eq = raw
        .find('=')
        .ok_or_else(|| format!("expected field=value or field:=json, got '{raw}'"))?;

    // `field:=json` — the `=` is immediately preceded by `:`.
    if eq > 0 && raw.as_bytes()[eq - 1] == b':' {
        let key = &raw[..eq - 1];
        let json_str = &raw[eq + 1..];
        if key.is_empty() {
            return Err(format!("empty field name in '{raw}'"));
        }
        let value: Value = serde_json::from_str(json_str)
            .map_err(|e| format!("invalid JSON for field '{key}': {e}"))?;
        return Ok((key.to_string(), value));
    }

    let key = &raw[..eq];
    let val = &raw[eq + 1..];
    if key.is_empty() {
        return Err(format!("empty field name in '{raw}'"));
    }
    Ok((key.to_string(), coerce_scalar(val)))
}

/// Infer a JSON scalar from a string, conservatively.
///
/// `true`/`false` become booleans; a string that round-trips exactly through
/// an `i64` or `f64` becomes a number; everything else stays a string. The
/// round-trip guard preserves leading zeros and oversized digit strings
/// (e.g. ZIP codes, phone numbers, IDs) as text, which matters because the
/// server coerces by schema and only accepts JSON numbers for numeric
/// fields. Use `field:=json` to force any specific JSON value.
fn coerce_scalar(raw: &str) -> Value {
    match raw {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        _ => {}
    }
    if let Ok(i) = raw.parse::<i64>() {
        if i.to_string() == raw {
            return Value::Number(i.into());
        }
    }
    if let Ok(f) = raw.parse::<f64>() {
        if f.is_finite() {
            if let Some(n) = serde_json::Number::from_f64(f) {
                if n.to_string() == raw {
                    return Value::Number(n);
                }
            }
        }
    }
    Value::String(raw.to_string())
}

/// Build the GET list query string from raw operands and typed flags.
fn list_query_params(args: &EntityListArgs) -> Result<Vec<(String, String)>, CliError> {
    let mut q: Vec<(String, String)> = Vec::new();

    // Raw `field__op=value` operands pass through verbatim.
    for raw in &args.filters {
        let (k, v) = split_field_value(raw)?;
        q.push((k.to_string(), v.to_string()));
    }

    push_op(&args.eq, "", &mut q)?;
    push_op(&args.ne, "__ne", &mut q)?;
    push_op(&args.gt, "__gt", &mut q)?;
    push_op(&args.gte, "__gte", &mut q)?;
    push_op(&args.lt, "__lt", &mut q)?;
    push_op(&args.lte, "__lte", &mut q)?;
    push_op(&args.contains, "__contains", &mut q)?;
    push_op(&args.startswith, "__startswith", &mut q)?;
    push_op(&args.in_, "__in", &mut q)?;

    if let Some(s) = &args.sort {
        q.push(("sort".to_string(), s.clone()));
    }
    if let Some(f) = &args.fields {
        q.push(("fields".to_string(), f.clone()));
    }
    if let Some(l) = args.limit {
        q.push(("limit".to_string(), l.to_string()));
    }
    if let Some(o) = args.offset {
        q.push(("offset".to_string(), o.to_string()));
    }
    if args.no_count {
        q.push(("count".to_string(), "false".to_string()));
    }
    if args.no_resolve {
        q.push(("resolve".to_string(), "false".to_string()));
    }
    Ok(q)
}

/// Append `field{suffix}=value` query pairs from a list of `field=value`
/// flag values.
fn push_op(
    values: &[String],
    suffix: &str,
    out: &mut Vec<(String, String)>,
) -> Result<(), CliError> {
    for raw in values {
        let (field, value) = split_field_value(raw)?;
        out.push((format!("{field}{suffix}"), value.to_string()));
    }
    Ok(())
}

/// Split `field=value` on the first `=`, rejecting an empty field.
fn split_field_value(raw: &str) -> Result<(&str, &str), CliError> {
    let (field, value) = raw.split_once('=').ok_or_else(|| CliError::Config {
        message: format!("expected field=value, got '{raw}'"),
    })?;
    if field.is_empty() {
        return Err(CliError::Config {
            message: format!("empty field name in '{raw}'"),
        });
    }
    Ok((field, value))
}

/// Build the POST `query` body. `filter` is raw JSON; `sort` is converted to
/// the structured `[{ "field", "order" }]` form the endpoint expects.
fn build_query_body(
    filter: Option<&str>,
    sort: Option<&str>,
    fields: Option<&str>,
    limit: Option<usize>,
    offset: Option<usize>,
    no_count: bool,
    no_resolve: bool,
) -> Result<Value, CliError> {
    let mut obj = Map::new();

    if let Some(f) = filter {
        let parsed: Value = serde_json::from_str(f).map_err(|e| CliError::Config {
            message: format!("--filter is not valid JSON: {e}"),
        })?;
        obj.insert("filter".to_string(), parsed);
    }
    if let Some(s) = sort {
        obj.insert("sort".to_string(), Value::Array(parse_sort_to_clauses(s)));
    }
    if let Some(f) = fields {
        let names: Vec<Value> = f
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| Value::String(s.to_string()))
            .collect();
        obj.insert("fields".to_string(), Value::Array(names));
    }
    if let Some(l) = limit {
        obj.insert("limit".to_string(), Value::from(l));
    }
    if let Some(o) = offset {
        obj.insert("offset".to_string(), Value::from(o));
    }
    if no_count {
        obj.insert("count".to_string(), Value::Bool(false));
    }
    if no_resolve {
        obj.insert("resolve".to_string(), Value::Bool(false));
    }
    Ok(Value::Object(obj))
}

/// Convert a `-age,name` / `age:desc` sort spec into the endpoint's
/// `[{ "field", "order" }]` array. Mirrors the server's `parse_sort_param`.
fn parse_sort_to_clauses(sort: &str) -> Vec<Value> {
    let mut clauses = Vec::new();
    for part in sort.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (field, order) = if let Some(rest) = part.strip_prefix('-') {
            (rest, "desc")
        } else if let Some(rest) = part.strip_prefix('+') {
            (rest, "asc")
        } else if let Some((field, dir)) = part.rsplit_once(':') {
            (field, if dir == "desc" { "desc" } else { "asc" })
        } else {
            (part, "asc")
        };
        if field.is_empty() {
            continue;
        }
        clauses.push(json!({ "field": field, "order": order }));
    }
    clauses
}

// ---------------------------------------------------------------------------
// Output rendering
// ---------------------------------------------------------------------------

/// Render a single-entity response per the active output mode.
fn render_entity(output: &OutputContext, value: &Value) -> Result<(), CliError> {
    if output.mode == OutputMode::Json {
        output.print_json(value);
        return Ok(());
    }
    let dto: EntityDto = serde_json::from_value(value.clone()).map_err(|e| CliError::Server {
        message: format!("unexpected entity response: {e}"),
    })?;
    let sep = if output.mode == OutputMode::Plain {
        '\t'
    } else {
        ':'
    };
    if output.mode == OutputMode::Plain {
        println!("id\t{}", dto.id);
    } else {
        println!("id: {}", dto.id);
    }
    for (k, v) in &dto.fields {
        if output.mode == OutputMode::Plain {
            println!("{k}{sep}{}", render_cell(v));
        } else {
            println!("{k}{sep} {}", render_cell(v));
        }
    }
    Ok(())
}

/// Emit a write result: `ok <verb> <id>` to stderr, the entity to stdout.
fn report_write(output: &OutputContext, verb: &str, value: &Value) -> Result<(), CliError> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    output.success(&format!("{verb} {id}"));
    render_entity(output, value)
}

/// Render a list/query response per the active output mode.
fn render_list(
    output: &OutputContext,
    value: &Value,
    fields: Option<&str>,
) -> Result<(), CliError> {
    if output.mode == OutputMode::Json {
        output.print_json(value);
        return Ok(());
    }
    let list: ListDto = serde_json::from_value(value.clone()).map_err(|e| CliError::Server {
        message: format!("unexpected list response: {e}"),
    })?;

    let projection: Option<Vec<String>> = fields.map(|f| {
        f.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    });
    let headers = list_columns(&list, projection.as_deref());
    let rows = list_rows(&list, &headers);

    match output.mode {
        OutputMode::Plain => print_tsv(&headers, &rows),
        _ => print_table(&headers, &rows),
    }

    match list.total_count {
        Some(total) => output.status(&format!("{} of {} entities", list.count, total)),
        None => output.status(&format!("{} entities", list.count)),
    }
    Ok(())
}

/// Choose table columns: `id` plus the projection, or the first entity's
/// fields (excluding resolved `__display` siblings to reduce noise).
fn list_columns(list: &ListDto, projection: Option<&[String]>) -> Vec<String> {
    let mut cols = vec!["id".to_string()];
    if let Some(proj) = projection {
        for f in proj {
            if f != "id" {
                cols.push(f.clone());
            }
        }
    } else if let Some(first) = list.entities.first() {
        for k in first.fields.keys() {
            if !k.ends_with("__display") {
                cols.push(k.clone());
            }
        }
    }
    cols
}

/// Project each entity onto the chosen columns.
fn list_rows(list: &ListDto, headers: &[String]) -> Vec<Vec<String>> {
    list.entities
        .iter()
        .map(|e| {
            headers
                .iter()
                .map(|h| {
                    if h == "id" {
                        e.id.clone()
                    } else {
                        e.fields.get(h).map(render_cell).unwrap_or_default()
                    }
                })
                .collect()
        })
        .collect()
}

/// Render a JSON value as a single table cell.
fn render_cell(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(_) | Value::Number(_) => v.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Print a left-aligned, space-padded table to stdout.
fn print_table(headers: &[String], rows: &[Vec<String>]) {
    let mut widths: Vec<usize> = headers.iter().map(String::len).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if let Some(w) = widths.get_mut(i) {
                *w = (*w).max(cell.len());
            }
        }
    }
    let render = |cells: &[String]| {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let w = widths.get(i).copied().unwrap_or(0);
                format!("{c:<w$}")
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    println!("{}", render(headers));
    for row in rows {
        println!("{}", render(row));
    }
}

/// Print tab-separated rows to stdout.
fn print_tsv(headers: &[String], rows: &[Vec<String>]) {
    println!("{}", headers.join("\t"));
    for row in rows {
        println!("{}", row.join("\t"));
    }
}

/// Print the resolved request without sending it (`--dry-run`).
fn dry_run_report(
    output: &OutputContext,
    rc: &ResolvedClient,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<(), CliError> {
    let url = format!("{}{}", forge_base(&rc.server, &rc.api_version), path);
    let report = json!({
        "dry_run": true,
        "method": method,
        "url": url,
        "body": body,
    });
    output.print_json(&report);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coerce_scalar_detects_int_bool_keeps_text() {
        assert_eq!(coerce_scalar("30"), json!(30));
        assert_eq!(coerce_scalar("-5"), json!(-5));
        assert_eq!(coerce_scalar("true"), json!(true));
        assert_eq!(coerce_scalar("false"), json!(false));
        assert_eq!(coerce_scalar("Alice"), json!("Alice"));
    }

    #[test]
    fn coerce_scalar_preserves_leading_zeros_and_big_ids() {
        // Round-trip guard: these must stay strings, not become numbers.
        assert_eq!(coerce_scalar("01234"), json!("01234"));
        assert_eq!(coerce_scalar("007"), json!("007"));
        assert_eq!(
            coerce_scalar("99999999999999999999999"),
            json!("99999999999999999999999")
        );
    }

    #[test]
    fn coerce_scalar_handles_floats() {
        assert_eq!(coerce_scalar("9.99"), json!(9.99));
        // Trailing-zero floats don't round-trip exactly; stay text (use :=).
        assert_eq!(coerce_scalar("2.70"), json!("2.70"));
    }

    #[test]
    fn parse_set_arg_typed_and_raw_json() {
        assert_eq!(parse_set_arg("name=Alice").unwrap(), ("name".into(), json!("Alice")));
        assert_eq!(parse_set_arg("age=30").unwrap(), ("age".into(), json!(30)));
        assert_eq!(
            parse_set_arg("tags:=[\"a\",\"b\"]").unwrap(),
            ("tags".into(), json!(["a", "b"]))
        );
        assert_eq!(
            parse_set_arg("owner:=\"user_1\"").unwrap(),
            ("owner".into(), json!("user_1"))
        );
    }

    #[test]
    fn parse_set_arg_value_can_contain_equals() {
        assert_eq!(
            parse_set_arg("note=a=b").unwrap(),
            ("note".into(), json!("a=b"))
        );
    }

    #[test]
    fn parse_set_arg_rejects_bad_input() {
        assert!(parse_set_arg("noequals").is_err());
        assert!(parse_set_arg("=value").is_err());
        assert!(parse_set_arg("bad:=not json").is_err());
    }

    #[test]
    fn build_fields_body_from_sets_wraps_envelope() {
        let body = build_fields_body(&["name=Alice".into(), "age=30".into()], None).unwrap();
        assert_eq!(body, json!({ "fields": { "name": "Alice", "age": 30 } }));
    }

    #[test]
    fn build_fields_body_data_bare_map_with_set_overlay() {
        let body =
            build_fields_body(&["age=31".into()], Some(r#"{"name":"Bob","age":30}"#)).unwrap();
        assert_eq!(body, json!({ "fields": { "name": "Bob", "age": 31 } }));
    }

    #[test]
    fn build_fields_body_accepts_already_wrapped_envelope() {
        let body = build_fields_body(&[], Some(r#"{"fields":{"name":"Cara"}}"#)).unwrap();
        assert_eq!(body, json!({ "fields": { "name": "Cara" } }));
    }

    #[test]
    fn build_fields_body_rejects_non_object_data() {
        assert!(build_fields_body(&[], Some("[1,2,3]")).is_err());
        assert!(build_fields_body(&[], Some("not json")).is_err());
    }

    #[test]
    fn split_field_value_splits_on_first_equals() {
        assert_eq!(split_field_value("age__gt=25").unwrap(), ("age__gt", "25"));
        assert_eq!(split_field_value("note=a=b").unwrap(), ("note", "a=b"));
        assert!(split_field_value("noequals").is_err());
        assert!(split_field_value("=v").is_err());
    }

    #[test]
    fn parse_sort_to_clauses_matches_server_grammar() {
        assert_eq!(
            parse_sort_to_clauses("-age,name"),
            vec![
                json!({ "field": "age", "order": "desc" }),
                json!({ "field": "name", "order": "asc" }),
            ]
        );
        assert_eq!(
            parse_sort_to_clauses("age:desc"),
            vec![json!({ "field": "age", "order": "desc" })]
        );
    }

    #[test]
    fn build_query_body_assembles_present_keys_only() {
        let body = build_query_body(
            Some(r#"{"op":"eq","field":"name","value":"Al"}"#),
            Some("-age"),
            Some("id,name"),
            Some(10),
            Some(5),
            true,
            false,
        )
        .unwrap();
        assert_eq!(body["filter"]["field"], "name");
        assert_eq!(body["sort"][0]["order"], "desc");
        assert_eq!(body["fields"], json!(["id", "name"]));
        assert_eq!(body["limit"], 10);
        assert_eq!(body["offset"], 5);
        assert_eq!(body["count"], false);
        assert!(body.get("resolve").is_none());
    }

    #[test]
    fn render_cell_formats_by_type() {
        assert_eq!(render_cell(&json!("hi")), "hi");
        assert_eq!(render_cell(&json!(42)), "42");
        assert_eq!(render_cell(&json!(true)), "true");
        assert_eq!(render_cell(&Value::Null), "");
        assert_eq!(render_cell(&json!(["a", "b"])), "[\"a\",\"b\"]");
    }

    #[test]
    fn list_columns_uses_projection_then_first_entity() {
        let list: ListDto = serde_json::from_value(json!({
            "entities": [{ "id": "x_1", "fields": { "name": "A", "age": 1, "boss__display": "B" } }],
            "count": 1
        }))
        .unwrap();
        // Projection drives columns (id always first).
        assert_eq!(
            list_columns(&list, Some(&["name".to_string(), "age".to_string()])),
            vec!["id", "name", "age"]
        );
        // No projection: first entity's fields, minus __display siblings.
        let cols = list_columns(&list, None);
        assert!(cols.contains(&"name".to_string()));
        assert!(cols.contains(&"age".to_string()));
        assert!(!cols.iter().any(|c| c.ends_with("__display")));
    }
}
