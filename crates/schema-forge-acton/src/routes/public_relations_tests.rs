use super::filter_public_relation_entities;
use crate::authz::{PolicyStore, PolicyStoreSnapshot, PrincipalClaimMappings, RoleRanks};
use schema_forge_backend::entity::Entity;
use schema_forge_core::types::{
    Annotation, DynamicValue, FieldAnnotation, FieldDefinition, FieldName, FieldType,
    SchemaDefinition, SchemaId, SchemaName, TextConstraints,
};
use std::collections::BTreeMap;
use std::sync::Arc;

fn schema(public: bool, protected: bool) -> SchemaDefinition {
    let mut label = FieldDefinition::new(
        FieldName::new("label").unwrap(),
        FieldType::Text(TextConstraints::unconstrained()),
    );
    if protected {
        label.annotations.push(FieldAnnotation::FieldAccess {
            read: vec!["staff".into()],
            write: vec!["staff".into()],
        });
    }
    let mut hidden = FieldDefinition::new(
        FieldName::new("secret").unwrap(),
        FieldType::Text(TextConstraints::unconstrained()),
    );
    hidden.annotations.push(FieldAnnotation::Hidden);
    SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Related").unwrap(),
        vec![
            label,
            hidden,
            FieldDefinition::new(FieldName::new("published").unwrap(), FieldType::Boolean),
        ],
        vec![Annotation::Access {
            read: vec![if public { "public" } else { "staff" }.into()],
            write: vec!["staff".into()],
            delete: vec!["staff".into()],
            cross_tenant_read: vec![],
        }],
    )
    .unwrap()
}

fn store(schema: &SchemaDefinition, policy: Option<&str>) -> Arc<PolicyStore> {
    let dir = tempfile::tempdir().unwrap();
    if let Some(policy) = policy {
        std::fs::write(dir.path().join("related.cedar"), policy).unwrap();
    }
    let snapshot = PolicyStoreSnapshot::from_schemas(
        std::slice::from_ref(schema),
        policy.map(|_| dir.path()),
        RoleRanks::empty(),
        PrincipalClaimMappings::default(),
    )
    .unwrap();
    Arc::new(PolicyStore::new(snapshot))
}

fn row(schema: &SchemaDefinition, published: bool) -> Entity {
    Entity::new(
        schema.name.clone(),
        BTreeMap::from([
            ("label".into(), DynamicValue::Text("Display name".into())),
            ("secret".into(), DynamicValue::Text("Never disclose".into())),
            ("published".into(), DynamicValue::Boolean(published)),
        ]),
    )
}

#[test]
fn anonymous_enrichment_cannot_disclose_private_target_rows() {
    let schema = schema(false, false);
    let rows =
        filter_public_relation_entities(&store(&schema, None), &schema, vec![row(&schema, true)]);
    assert!(
        rows.is_empty(),
        "private target IDs and displays must be absent"
    );
}

#[test]
fn anonymous_enrichment_checks_full_rows_before_selecting_display_or_child_ids() {
    let schema = schema(true, false);
    let policy = r#"
forbid(principal, action == Action::"ReadRelated", resource is Related)
when { resource has published && !resource.published };
"#;
    let published = row(&schema, true);
    let id = published.id.clone();
    let rows = filter_public_relation_entities(
        &store(&schema, Some(policy)),
        &schema,
        vec![row(&schema, false), published],
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, id);
    assert!(rows[0].field("label").is_some());
    assert!(rows[0].field("secret").is_none());
}

#[test]
fn anonymous_enrichment_scrubs_restricted_display_and_foreign_key_values() {
    let schema = schema(true, true);
    let rows =
        filter_public_relation_entities(&store(&schema, None), &schema, vec![row(&schema, true)]);
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].field("label").is_none(),
        "restricted values cannot form display labels or parent groups"
    );
    assert!(rows[0].field("secret").is_none());
}

struct AuthenticatedOnlyPolicy;

impl schema_forge_backend::auth::RecordAccessPolicy for AuthenticatedOnlyPolicy {
    fn filter_visible<'a>(
        &'a self,
        _schema: &'a SchemaDefinition,
        _claims: &'a acton_service::middleware::Claims,
        entities: Vec<Entity>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<Entity>> + Send + 'a>> {
        Box::pin(async move { entities })
    }

    fn can_modify<'a>(
        &'a self,
        _schema: &'a SchemaDefinition,
        _claims: &'a acton_service::middleware::Claims,
        _entity: &'a Entity,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async { false })
    }

    fn can_delete<'a>(
        &'a self,
        _schema: &'a SchemaDefinition,
        _claims: &'a acton_service::middleware::Claims,
        _entity: &'a Entity,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async { false })
    }
}

#[tokio::test]
async fn anonymous_enrichment_honors_custom_record_policy_denial() {
    let schema = schema(true, false);
    let store = store(&schema, None);
    let entity = row(&schema, true);
    let without_custom = super::filter_public_relation_entities_with_policy(
        None,
        &store,
        &schema,
        vec![entity.clone()],
    )
    .await;
    assert_eq!(without_custom.len(), 1, "Cedar permits this target");
    let restricted = super::filter_public_relation_entities_with_policy(
        Some(&AuthenticatedOnlyPolicy),
        &store,
        &schema,
        vec![entity],
    )
    .await;
    assert!(
        restricted.is_empty(),
        "custom record denial must prevent display and child ID disclosure"
    );
}
