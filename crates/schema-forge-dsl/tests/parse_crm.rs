use schema_forge_core::types::{Cardinality, FieldModifier, FieldType};
use schema_forge_dsl::parse;

/// The full CRM schema from the SchemaForge plan document.
const CRM_SCHEMA: &str = r#"
// Line comments
/* Block comments */

schema Contact {
    name:            text(max: 255) required indexed
    email:           text(max: 512) required indexed
    phone:           text
    priority:        enum("low", "medium", "high") default("medium")
    company:         -> Company
    deals:           -> Deal[]
    tags:            text[]
    notes:           richtext
    last_contacted:  datetime
    score:           integer(min: 0, max: 100)
    annual_revenue:  float(precision: 2)
    is_active:       boolean default(true)
    metadata:        json
}

schema Company {
    name:            text(max: 255) required indexed
    industry:        enum("fintech", "saas", "healthcare", "other")
    website:         text
    employee_count:  integer(min: 1)
    founded:         datetime
    contacts:        -> Contact[]
    address:         composite {
        street:      text
        city:        text required
        state:       text
        zip:         text
        country:     text required
    }
}

// Schema-level configuration
@version(2)
@display("name")
schema Deal {
    name:            text(max: 255) required
    value:           float(precision: 2) required
    stage:           enum("prospect", "qualified", "proposal", "closed_won", "closed_lost")
    contact:         -> Contact required
    company:         -> Company required
    expected_close:  datetime
    notes:           richtext
}
"#;

#[test]
fn parse_full_crm_schema() {
    let schemas = parse(CRM_SCHEMA).expect("CRM schema should parse successfully");
    assert_eq!(
        schemas.len(),
        3,
        "expected 3 schemas: Contact, Company, Deal"
    );

    // --- Contact ---
    let contact = &schemas[0];
    assert_eq!(contact.name.as_str(), "Contact");
    assert_eq!(contact.fields.len(), 13);
    assert!(contact.annotations.is_empty());

    // name field
    let name = contact.field("name").expect("Contact.name");
    assert!(name.is_required());
    assert!(name.is_indexed());
    match &name.field_type {
        FieldType::Text(c) => assert_eq!(c.max_length, Some(255)),
        other => panic!("expected Text, got {other:?}"),
    }

    // email field
    let email = contact.field("email").expect("Contact.email");
    assert!(email.is_required());
    assert!(email.is_indexed());
    match &email.field_type {
        FieldType::Text(c) => assert_eq!(c.max_length, Some(512)),
        other => panic!("expected Text, got {other:?}"),
    }

    // phone field - no modifiers
    let phone = contact.field("phone").expect("Contact.phone");
    assert!(!phone.is_required());
    assert!(matches!(phone.field_type, FieldType::Text(_)));

    // priority enum with default
    let priority = contact.field("priority").expect("Contact.priority");
    match &priority.field_type {
        FieldType::Enum(variants) => {
            assert_eq!(variants.as_slice(), &["low", "medium", "high"]);
        }
        other => panic!("expected Enum, got {other:?}"),
    }
    assert!(matches!(
        &priority.modifiers[0],
        FieldModifier::Default {
            value: schema_forge_core::types::DefaultValue::String(s)
        } if s == "medium"
    ));

    // company relation one
    let company = contact.field("company").expect("Contact.company");
    match &company.field_type {
        FieldType::Relation {
            target,
            cardinality,
        } => {
            assert_eq!(target.as_str(), "Company");
            assert_eq!(*cardinality, Cardinality::One);
        }
        other => panic!("expected Relation, got {other:?}"),
    }

    // deals relation many
    let deals = contact.field("deals").expect("Contact.deals");
    match &deals.field_type {
        FieldType::Relation {
            target,
            cardinality,
        } => {
            assert_eq!(target.as_str(), "Deal");
            assert_eq!(*cardinality, Cardinality::Many);
        }
        other => panic!("expected Relation, got {other:?}"),
    }

    // tags array
    let tags = contact.field("tags").expect("Contact.tags");
    assert!(
        matches!(&tags.field_type, FieldType::Array(inner) if matches!(inner.as_ref(), FieldType::Text(_)))
    );

    // notes richtext
    let notes = contact.field("notes").expect("Contact.notes");
    assert!(matches!(notes.field_type, FieldType::RichText));

    // last_contacted datetime
    let lc = contact
        .field("last_contacted")
        .expect("Contact.last_contacted");
    assert!(matches!(lc.field_type, FieldType::DateTime));

    // score integer
    let score = contact.field("score").expect("Contact.score");
    match &score.field_type {
        FieldType::Integer(c) => {
            assert_eq!(c.min, Some(0));
            assert_eq!(c.max, Some(100));
        }
        other => panic!("expected Integer, got {other:?}"),
    }

    // annual_revenue float
    let rev = contact
        .field("annual_revenue")
        .expect("Contact.annual_revenue");
    match &rev.field_type {
        FieldType::Float(c) => assert_eq!(c.precision, Some(2)),
        other => panic!("expected Float, got {other:?}"),
    }

    // is_active boolean with default
    let active = contact.field("is_active").expect("Contact.is_active");
    assert!(matches!(active.field_type, FieldType::Boolean));
    assert!(matches!(
        &active.modifiers[0],
        FieldModifier::Default {
            value: schema_forge_core::types::DefaultValue::Boolean(true)
        }
    ));

    // metadata json
    let metadata = contact.field("metadata").expect("Contact.metadata");
    assert!(matches!(metadata.field_type, FieldType::Json));

    // --- Company ---
    let company = &schemas[1];
    assert_eq!(company.name.as_str(), "Company");
    assert_eq!(company.fields.len(), 7);

    // Composite address
    let address = company.field("address").expect("Company.address");
    match &address.field_type {
        FieldType::Composite(fields) => {
            assert_eq!(fields.len(), 5);
            assert_eq!(fields[0].name.as_str(), "street");
            assert_eq!(fields[1].name.as_str(), "city");
            assert!(fields[1].is_required());
            assert_eq!(fields[2].name.as_str(), "state");
            assert_eq!(fields[3].name.as_str(), "zip");
            assert_eq!(fields[4].name.as_str(), "country");
            assert!(fields[4].is_required());
        }
        other => panic!("expected Composite, got {other:?}"),
    }

    // --- Deal ---
    let deal = &schemas[2];
    assert_eq!(deal.name.as_str(), "Deal");
    assert_eq!(deal.fields.len(), 7);
    assert_eq!(deal.annotations.len(), 2);

    // Verify annotations
    match &deal.annotations[0] {
        schema_forge_core::types::Annotation::Version { version } => {
            assert_eq!(version.get(), 2);
        }
        other => panic!("expected Version, got {other:?}"),
    }
    match &deal.annotations[1] {
        schema_forge_core::types::Annotation::Display { field } => {
            assert_eq!(field.as_str(), "name");
        }
        other => panic!("expected Display, got {other:?}"),
    }

    // contact relation with required modifier
    let deal_contact = deal.field("contact").expect("Deal.contact");
    assert!(deal_contact.is_required());
    assert!(matches!(
        &deal_contact.field_type,
        FieldType::Relation {
            target,
            cardinality: Cardinality::One
        } if target.as_str() == "Contact"
    ));
}

#[test]
fn parse_single_schema_minimal() {
    let schemas = parse("schema Minimal { x: boolean }").unwrap();
    assert_eq!(schemas.len(), 1);
    assert_eq!(schemas[0].name.as_str(), "Minimal");
}

#[test]
fn parse_empty_input_returns_empty_list() {
    let schemas = parse("").unwrap();
    assert!(schemas.is_empty());
}

#[test]
fn parse_comments_only_returns_empty_list() {
    let schemas = parse("// just comments\n/* block */").unwrap();
    assert!(schemas.is_empty());
}
