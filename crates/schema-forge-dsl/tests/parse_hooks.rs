//! Tests for `@hook` annotation parsing.

use schema_forge_core::types::{Annotation, HookEvent};
use schema_forge_dsl::parse;

fn parse_one_schema(source: &str) -> schema_forge_core::types::SchemaDefinition {
    let mut schemas = parse(source).expect("parse should succeed");
    assert_eq!(schemas.len(), 1);
    schemas.remove(0)
}

fn hook_for_event(
    schema: &schema_forge_core::types::SchemaDefinition,
    event: HookEvent,
) -> &Annotation {
    schema
        .annotations
        .iter()
        .find(|a| matches!(a, Annotation::Hook { event: e, .. } if *e == event))
        .unwrap_or_else(|| panic!("expected hook for {:?}", event))
}

#[test]
fn parse_hook_before_change() {
    let s = parse_one_schema(
        r#"@hook(before_change) """Validate input."""
           schema S { name: text }"#,
    );
    let hook = hook_for_event(&s, HookEvent::BeforeChange);
    assert!(matches!(hook, Annotation::Hook { intent, .. } if intent == "Validate input."));
}

#[test]
fn parse_hook_all_events() {
    let s = parse_one_schema(
        r#"@hook(before_validate) """a"""
           @hook(before_change) """b"""
           @hook(after_change) """c"""
           @hook(before_read) """d"""
           @hook(after_read) """e"""
           @hook(before_delete) """f"""
           @hook(after_delete) """g"""
           @hook(before_upload) """h"""
           @hook(after_upload) """i"""
           @hook(on_scan_complete) """j"""
           schema S { name: text }"#,
    );
    for ev in HookEvent::ALL {
        let _ = hook_for_event(&s, *ev);
    }
}

#[test]
fn parse_hook_unknown_event_rejected() {
    let result = parse(r#"@hook(bogus) """x""" schema S { name: text }"#);
    let errors = result.expect_err("unknown event should fail");
    let msg = errors[0].to_string();
    assert!(
        msg.contains("'bogus'"),
        "error should mention the bad event name: {msg}"
    );
}

#[test]
fn parse_hook_missing_intent_rejected() {
    let result = parse("@hook(before_change) schema S { name: text }");
    let errors = result.expect_err("missing intent should fail");
    let msg = errors[0].to_string();
    assert!(
        msg.contains("triple-quoted"),
        "error should mention triple-quoted: {msg}"
    );
}

#[test]
fn parse_hook_unterminated_intent_rejected() {
    // Missing closing triple-quote -- the lexer should fail.
    let result = parse(r#"@hook(before_change) """oops schema S { name: text }"#);
    assert!(result.is_err());
}

#[test]
fn parse_hook_duplicate_event_rejected() {
    // Two @hook(before_change) on the same schema -- caught by the
    // duplicate-annotation kind check in SchemaDefinition::new.
    let result = parse(
        r#"@hook(before_change) """first"""
           @hook(before_change) """second"""
           schema S { name: text }"#,
    );
    let errors = result.expect_err("duplicate hook events should fail");
    let msg = errors[0].to_string();
    assert!(
        msg.contains("duplicate") || msg.contains("hook:before_change"),
        "expected duplicate-annotation error, got: {msg}"
    );
}

#[test]
fn parse_hook_distinct_events_allowed() {
    let s = parse_one_schema(
        r#"@hook(before_change) """v"""
           @hook(after_change) """w"""
           schema S { name: text }"#,
    );
    assert_eq!(s.annotations.len(), 2);
}

#[test]
fn parse_hook_multiline_intent() {
    let s = parse_one_schema(
        "@hook(before_change) \"\"\"\n  one\n  two\n  three\n\"\"\" schema S { name: text }",
    );
    let hook = hook_for_event(&s, HookEvent::BeforeChange);
    let intent = match hook {
        Annotation::Hook { intent, .. } => intent,
        _ => unreachable!(),
    };
    assert!(intent.contains("one"));
    assert!(intent.contains("three"));
}

#[test]
fn parse_hook_intent_preserves_internal_double_quotes() {
    let s = parse_one_schema(
        r#"@hook(before_change) """She said "hi" then left.""" schema S { name: text }"#,
    );
    let hook = hook_for_event(&s, HookEvent::BeforeChange);
    let intent = match hook {
        Annotation::Hook { intent, .. } => intent,
        _ => unreachable!(),
    };
    assert!(intent.contains(r#""hi""#));
}
