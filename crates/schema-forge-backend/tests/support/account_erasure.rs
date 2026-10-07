//! Account erasure behavioral contract, including exact related-row counts.
use schema_forge_backend::{AccountErasureCounts, Entity, EntityStore, SchemaBackend};
use schema_forge_core::{
    migration::DiffEngine,
    types::{
        Cardinality, DynamicValue, FieldDefinition, FieldName, FieldType, SchemaDefinition,
        SchemaId, SchemaName, TextConstraints,
    },
};
use std::collections::BTreeMap;

pub struct Fixture {
    pub user: Entity,
    pub related: Vec<Entity>,
    pub other: Vec<Entity>,
}

fn text() -> FieldType {
    FieldType::Text(TextConstraints::unconstrained())
}
fn user_ref() -> FieldType {
    FieldType::Relation {
        target: SchemaName::new("User").unwrap(),
        cardinality: Cardinality::One,
    }
}
fn row(table: &str, fields: Vec<(&str, DynamicValue)>) -> Entity {
    Entity::new(
        SchemaName::new(table).unwrap(),
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect::<BTreeMap<_, _>>(),
    )
}

pub async fn setup(backend: &(impl EntityStore + SchemaBackend)) -> Fixture {
    for (table, fields) in [
        ("User", vec![("email", text())]),
        (
            "OAuthIdentity",
            vec![("user", user_ref()), ("subject", text())],
        ),
        (
            "TenantMembership",
            vec![("user", user_ref()), ("tenant_id", text())],
        ),
        (
            "ForgeInvitation",
            vec![("email", text()), ("status", text()), ("token", text())],
        ),
    ] {
        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new(table).unwrap(),
            fields
                .into_iter()
                .map(|(name, kind)| FieldDefinition::new(FieldName::new(name).unwrap(), kind))
                .collect(),
            vec![],
        )
        .unwrap();
        backend
            .apply_schema_change(
                &schema.name,
                &DiffEngine::create_new(&schema).steps,
                Some(&schema),
            )
            .await
            .unwrap();
    }
    let user = backend
        .create(&row(
            "User",
            vec![("email", DynamicValue::Text("erase@example.gov".into()))],
        ))
        .await
        .unwrap();
    let other_user = backend
        .create(&row(
            "User",
            vec![("email", DynamicValue::Text("keep@example.gov".into()))],
        ))
        .await
        .unwrap();
    let dependants = |user: &Entity, email: &str| {
        vec![
            row(
                "OAuthIdentity",
                vec![
                    ("user", DynamicValue::Ref(user.id.clone())),
                    ("subject", DynamicValue::Text(user.id.to_string())),
                ],
            ),
            row(
                "TenantMembership",
                vec![
                    ("user", DynamicValue::Ref(user.id.clone())),
                    ("tenant_id", DynamicValue::Text("tenant".into())),
                ],
            ),
            row(
                "ForgeInvitation",
                vec![
                    ("email", DynamicValue::Text(email.into())),
                    ("status", DynamicValue::Text("consumed".into())),
                    ("token", DynamicValue::Text("private-token".into())),
                ],
            ),
        ]
    };
    let mut related = dependants(&user, "erase@example.gov");
    related.push(row(
        "ForgeInvitation",
        vec![
            ("email", DynamicValue::Text("erase@example.gov".into())),
            ("status", DynamicValue::Text("pending".into())),
            ("token", DynamicValue::Text("pending-token".into())),
        ],
    ));
    let mut other = dependants(&other_user, "keep@example.gov");
    for entity in related.iter().chain(other.iter()) {
        backend.create(entity).await.unwrap();
    }
    other.push(other_user);
    Fixture {
        user,
        related,
        other,
    }
}

pub async fn assert_preserved(backend: &impl EntityStore, fixture: &Fixture) {
    assert!(backend
        .get(&fixture.user.schema, &fixture.user.id)
        .await
        .is_ok());
    for entity in fixture.related.iter().chain(fixture.other.iter()) {
        assert!(
            backend.get(&entity.schema, &entity.id).await.is_ok(),
            "{} must survive rollback",
            entity.schema
        );
    }
}

pub async fn erase(backend: &impl EntityStore, fixture: &Fixture) {
    assert_eq!(
        backend.erase_account(&fixture.user.id).await.unwrap(),
        AccountErasureCounts {
            identities: 1,
            memberships: 1,
            invitations: 2
        }
    );
    assert!(backend
        .get(&fixture.user.schema, &fixture.user.id)
        .await
        .is_err());
    for entity in &fixture.related {
        assert!(backend.get(&entity.schema, &entity.id).await.is_err());
    }
    for entity in &fixture.other {
        assert!(backend.get(&entity.schema, &entity.id).await.is_ok());
    }
    assert_eq!(
        backend.erase_account(&fixture.user.id).await.unwrap(),
        AccountErasureCounts::default()
    );
    assert_eq!(
        backend
            .delete_invitations_by_email("keep@example.gov")
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        backend
            .delete_invitations_by_email("keep@example.gov")
            .await
            .unwrap(),
        0
    );
    for entity in fixture
        .other
        .iter()
        .filter(|entity| entity.schema.as_str() != "ForgeInvitation")
    {
        assert!(backend.get(&entity.schema, &entity.id).await.is_ok());
    }
}
