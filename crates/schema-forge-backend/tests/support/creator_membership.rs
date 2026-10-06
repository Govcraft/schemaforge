//! Atomic onboarding behavior contract exercised by every shipping adapter.
use schema_forge_backend::{Entity, EntityStore, SchemaBackend};
use schema_forge_core::{
    migration::DiffEngine,
    types::{
        Cardinality, DynamicValue, EntityId, FieldDefinition, FieldName, FieldType,
        SchemaDefinition, SchemaId, SchemaName, TextConstraints,
    },
};
use std::collections::BTreeMap;

fn schema(name: &str, fields: Vec<(&str, FieldType)>) -> SchemaDefinition {
    SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new(name).unwrap(),
        fields
            .into_iter()
            .map(|(name, kind)| FieldDefinition::new(FieldName::new(name).unwrap(), kind))
            .collect(),
        vec![],
    )
    .unwrap()
}

fn text() -> FieldType {
    FieldType::Text(TextConstraints::unconstrained())
}

pub async fn setup(backend: &(impl EntityStore + SchemaBackend)) -> Entity {
    for schema in [
        schema("User", vec![("email", text())]),
        schema("Organization", vec![("name", text())]),
        schema(
            "TenantMembership",
            vec![
                (
                    "user",
                    FieldType::Relation {
                        target: SchemaName::new("User").unwrap(),
                        cardinality: Cardinality::One,
                    },
                ),
                ("tenant_type", text()),
                ("tenant_id", text()),
                ("role", text()),
            ],
        ),
    ] {
        backend
            .apply_schema_change(
                &schema.name,
                &DiffEngine::create_new(&schema).steps,
                Some(&schema),
            )
            .await
            .unwrap();
        if backend.supports_record_revisions() {
            backend
                .prepare_record_revisions(&schema.name)
                .await
                .unwrap();
        }
    }
    backend
        .create(&Entity::with_id(
            EntityId::new("entity"),
            SchemaName::new("User").unwrap(),
            BTreeMap::from([(
                "email".into(),
                DynamicValue::Text("owner@example.org".into()),
            )]),
        ))
        .await
        .unwrap()
}

pub fn root(name: &str) -> Entity {
    Entity::new(
        SchemaName::new("Organization").unwrap(),
        BTreeMap::from([("name".into(), DynamicValue::Text(name.into()))]),
    )
}

pub fn membership(root: &Entity, user: &Entity) -> Entity {
    Entity::new(
        SchemaName::new("TenantMembership").unwrap(),
        BTreeMap::from([
            ("user".into(), DynamicValue::Ref(user.id.clone())),
            (
                "tenant_type".into(),
                DynamicValue::Text(root.schema.to_string()),
            ),
            ("tenant_id".into(), DynamicValue::Text(root.id.to_string())),
            ("role".into(), DynamicValue::Text("owner".into())),
        ]),
    )
}

pub async fn exercise(backend: &(impl EntityStore + SchemaBackend)) {
    let user = setup(backend).await;
    let first = root("first");
    let grant = membership(&first, &user);
    assert_eq!(
        backend
            .create_with_membership(&first, &grant)
            .await
            .unwrap()
            .id,
        first.id
    );
    assert_eq!(backend.get(&grant.schema, &grant.id).await.unwrap(), grant);
    if backend.supports_record_revisions() {
        backend
            .get_versioned(&first.schema, &first.id)
            .await
            .unwrap();
        backend
            .get_versioned(&grant.schema, &grant.id)
            .await
            .unwrap();
    }
    // Root is inserted first, so a duplicate membership must roll it back.
    let second = root("rollback");
    let mut duplicate = membership(&second, &user);
    duplicate.id = grant.id.clone();
    assert!(backend
        .create_with_membership(&second, &duplicate)
        .await
        .is_err());
    assert!(backend.get(&second.schema, &second.id).await.is_err());
    assert_eq!(backend.get(&grant.schema, &grant.id).await.unwrap(), grant);
    if backend.supports_record_revisions() {
        assert!(backend
            .get_versioned(&second.schema, &second.id)
            .await
            .is_err());
    }
    let third = root("bad link");
    let mut bad_link = membership(&first, &user);
    assert!(backend
        .create_with_membership(&third, &bad_link)
        .await
        .is_err());
    assert!(backend.get(&third.schema, &third.id).await.is_err());
    assert!(backend.get(&bad_link.schema, &bad_link.id).await.is_err());
    // A valid-looking User ID must exist when the transaction commits.
    bad_link = membership(&third, &Entity::new(user.schema.clone(), BTreeMap::new()));
    assert!(backend
        .create_with_membership(&third, &bad_link)
        .await
        .is_err());
    assert!(backend.get(&third.schema, &third.id).await.is_err());
    assert!(backend.get(&bad_link.schema, &bad_link.id).await.is_err());
}
