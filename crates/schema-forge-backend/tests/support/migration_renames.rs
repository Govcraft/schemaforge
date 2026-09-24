use schema_forge_backend::{Entity, EntityStore, SchemaBackend};
use schema_forge_core::{
    migration::DiffEngine,
    types::{
        DynamicValue, FieldAnnotation, FieldDefinition, FieldModifier, FieldName, FieldType,
        SchemaDefinition, SchemaId, SchemaName, TextConstraints,
    },
};
use std::collections::BTreeMap;

pub async fn exercise<B: SchemaBackend + EntityStore>(backend: &B) {
    let old = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("RenameProbe").unwrap(),
        vec![
            FieldDefinition::with_modifiers(
                FieldName::new("number").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![
                    FieldModifier::Required,
                    FieldModifier::Unique,
                    FieldModifier::Indexed,
                ],
            ),
            FieldDefinition::new(FieldName::new("payload").unwrap(), FieldType::Json),
            FieldDefinition::new(FieldName::new("unrelated").unwrap(), FieldType::Json),
        ],
        vec![],
    )
    .unwrap();
    backend
        .apply_migration(&old.name, &DiffEngine::create_new(&old).steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&old).await.unwrap();
    let nested =
        DynamicValue::Json(serde_json::json!({"nested": [null, false, 12, {"quoted": "a\"b"}]}));
    let row = backend
        .create(&Entity::new(
            old.name.clone(),
            BTreeMap::from([
                ("number".into(), DynamicValue::Text("555-9999".into())),
                ("payload".into(), nested.clone()),
                ("unrelated".into(), nested.clone()),
            ]),
        ))
        .await
        .unwrap();
    let stored_nested = row.fields.get("payload").cloned().unwrap();
    let mut new = old.clone();
    for (field, target) in new.fields.iter_mut().zip(["business_number", "document"]) {
        field.annotations.push(FieldAnnotation::RenamedFrom {
            name: field.name.clone(),
        });
        field.name = FieldName::new(target).unwrap();
    }
    DiffEngine::validate_transition(&old, &new).unwrap();
    backend
        .apply_migration(&new.name, &DiffEngine::diff(&old, &new).steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&new).await.unwrap();
    let loaded = backend.get(&new.name, &row.id).await.unwrap();
    assert_eq!(
        loaded.fields.get("business_number"),
        Some(&DynamicValue::Text("555-9999".into()))
    );
    assert_eq!(loaded.fields.get("document"), Some(&stored_nested));
    assert_eq!(loaded.fields.get("unrelated"), row.fields.get("unrelated"));
    assert!(!loaded.fields.contains_key("number"));
    assert!(!loaded.fields.contains_key("payload"));
    assert!(DiffEngine::diff(&new, &new).is_empty());
    backend.store_schema_metadata(&new).await.unwrap();
    // A later ordinary write must retain both renamed and unrelated objects.
    let updated = backend
        .update(&Entity {
            id: row.id.clone(),
            schema: new.name.clone(),
            fields: BTreeMap::from([(
                "business_number".into(),
                DynamicValue::Text("555-0000".into()),
            )]),
        })
        .await
        .unwrap();
    assert_eq!(updated.fields.get("document"), Some(&stored_nested));
    assert_eq!(updated.fields.get("unrelated"), row.fields.get("unrelated"));
}
