use schema_forge_dsl::{parse, print, print_all};

/// Helper: parse source, print it, parse again, and compare the two ASTs.
///
/// We cannot compare `SchemaDefinition` directly because `SchemaId` is
/// randomly generated. Instead we compare field-by-field structural equality.
fn assert_round_trip(source: &str) {
    let schemas1 = parse(source).expect("first parse should succeed");
    let printed = print_all(&schemas1);
    let schemas2 = parse(&printed).unwrap_or_else(|errors| {
        panic!(
            "second parse (after printing) failed with errors: {errors:?}\n\nPrinted DSL:\n{printed}"
        );
    });

    assert_eq!(
        schemas1.len(),
        schemas2.len(),
        "schema count mismatch after round trip"
    );

    for (s1, s2) in schemas1.iter().zip(schemas2.iter()) {
        assert_eq!(s1.name, s2.name, "schema name mismatch");
        assert_eq!(
            s1.fields.len(),
            s2.fields.len(),
            "field count mismatch for schema '{}'",
            s1.name
        );

        for (f1, f2) in s1.fields.iter().zip(s2.fields.iter()) {
            assert_eq!(
                f1.name, f2.name,
                "field name mismatch in schema '{}'",
                s1.name
            );
            assert_eq!(
                f1.field_type, f2.field_type,
                "field type mismatch for '{}.{}'",
                s1.name, f1.name
            );
            assert_eq!(
                f1.modifiers, f2.modifiers,
                "modifier mismatch for '{}.{}'",
                s1.name, f1.name
            );
        }

        assert_eq!(
            s1.annotations.len(),
            s2.annotations.len(),
            "annotation count mismatch for schema '{}'",
            s1.name
        );
        for (a1, a2) in s1.annotations.iter().zip(s2.annotations.iter()) {
            assert_eq!(
                a1, a2,
                "annotation mismatch in schema '{}'",
                s1.name
            );
        }
    }
}

#[test]
fn round_trip_minimal_schema() {
    assert_round_trip("schema S { name: text }");
}

#[test]
fn round_trip_all_primitive_types() {
    assert_round_trip(
        r#"schema AllTypes {
            a: text
            b: text(max: 100)
            c: richtext
            d: integer
            e: integer(min: 0, max: 100)
            f: float
            g: float(precision: 4)
            h: boolean
            i: datetime
            j: json
        }"#,
    );
}

#[test]
fn round_trip_enum() {
    assert_round_trip(r#"schema S { status: enum("a", "b", "c") }"#);
}

#[test]
fn round_trip_relations() {
    assert_round_trip(
        "schema S {
            one: -> Target
            many: -> Target[]
        }",
    );
}

#[test]
fn round_trip_array_types() {
    assert_round_trip(
        "schema S {
            tags: text[]
            scores: integer[]
        }",
    );
}

#[test]
fn round_trip_composite() {
    assert_round_trip(
        "schema S {
            address: composite {
                street: text
                city: text required
                zip: text(max: 10)
            }
        }",
    );
}

#[test]
fn round_trip_modifiers() {
    assert_round_trip(
        r#"schema S {
            a: text required
            b: text indexed
            c: text required indexed
            d: text default("hello")
            e: integer default(42)
            f: boolean default(true)
        }"#,
    );
}

#[test]
fn round_trip_annotations() {
    assert_round_trip(
        r#"@version(3)
        @display("title")
        schema S { title: text }"#,
    );
}

#[test]
fn round_trip_multiple_schemas() {
    assert_round_trip(
        r#"schema Contact {
            name: text required
            email: text required indexed
        }

        schema Company {
            name: text required
            industry: enum("tech", "finance")
        }"#,
    );
}

#[test]
fn round_trip_full_crm() {
    assert_round_trip(
        r#"schema Contact {
            name: text(max: 255) required indexed
            email: text(max: 512) required indexed
            phone: text
            priority: enum("low", "medium", "high") default("medium")
            company: -> Company
            deals: -> Deal[]
            tags: text[]
            notes: richtext
            last_contacted: datetime
            score: integer(min: 0, max: 100)
            annual_revenue: float(precision: 2)
            is_active: boolean default(true)
            metadata: json
        }

        schema Company {
            name: text(max: 255) required indexed
            industry: enum("fintech", "saas", "healthcare", "other")
            website: text
            employee_count: integer(min: 1)
            founded: datetime
            contacts: -> Contact[]
            address: composite {
                street: text
                city: text required
                state: text
                zip: text
                country: text required
            }
        }

        @version(2)
        @display("name")
        schema Deal {
            name: text(max: 255) required
            value: float(precision: 2) required
            stage: enum("prospect", "qualified", "proposal", "closed_won", "closed_lost")
            contact: -> Contact required
            company: -> Company required
            expected_close: datetime
            notes: richtext
        }"#,
    );
}

#[test]
fn print_then_parse_preserves_comments_stripped() {
    // Comments are stripped during lexing, so they won't survive round-trip.
    // But the structural data should.
    let source = "// This is a comment\nschema S { name: text /* inline */ required }";
    let schemas = parse(source).unwrap();
    let printed = print(&schemas[0]);
    let reparsed = parse(&printed).unwrap();
    assert_eq!(reparsed[0].fields[0].name.as_str(), "name");
    assert!(reparsed[0].fields[0].is_required());
}
