//! Shared retention contract on real storage, including concurrent acceptance.
use chrono::{DateTime, TimeDelta, Utc};
use schema_forge_backend::{
    invite_store::{InvitationPruneCandidate, FORGE_INVITATION_SCHEMA},
    EntityInviteStore, EntityStore, InviteStore, NewInvitation, SchemaBackend,
};
use schema_forge_core::{migration::DiffEngine, types::DynamicValue};
use std::sync::Arc;

fn invitation(jti: &str, expires: DateTime<Utc>) -> NewInvitation {
    NewInvitation {
        email: "invitee@example.gov".into(),
        display_name: Some("Invitee".into()),
        tenant_type: None,
        tenant_id: None,
        role: None,
        jti: jti.into(),
        token: "v4.local.private-invite-material".into(),
        expires_at: expires,
        invited_by: Some("user:operator".into()),
    }
}

pub async fn exercise<B: SchemaBackend + EntityStore + 'static>(backend: Arc<B>) {
    let schema = schema_forge_dsl::parse(FORGE_INVITATION_SCHEMA)
        .unwrap()
        .remove(0);
    backend
        .apply_migration(&schema.name, &DiffEngine::create_new(&schema).steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&schema).await.unwrap();
    let store = EntityInviteStore::new(backend.clone(), schema.clone());
    let now = DateTime::parse_from_rfc3339("2026-10-07T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let cutoff = now - TimeDelta::days(30);
    let old = cutoff - TimeDelta::days(1);
    let live = now + TimeDelta::days(7);
    store.create(invitation("old-expired", old)).await.unwrap();
    let consumed = store
        .create(invitation("old-consumed", live))
        .await
        .unwrap();
    store.mark_consumed(&consumed.id, old).await.unwrap();
    store.create(invitation("live", live)).await.unwrap();
    let recent = store
        .create(invitation("recently-consumed", old))
        .await
        .unwrap();
    store
        .mark_consumed(&recent.id, now - TimeDelta::days(1))
        .await
        .unwrap();
    let boundary = store.create(invitation("boundary", cutoff)).await.unwrap();
    let malformed = store.create(invitation("malformed", old)).await.unwrap();
    let mut row = backend.get(&schema.name, &malformed.id).await.unwrap();
    row.fields
        .insert("expires_at".into(), DynamicValue::Text("invalid".into()));
    backend.update(&row).await.unwrap();

    let raced = store
        .create(invitation("raced-acceptance", old))
        .await
        .unwrap();
    let row = backend.get(&schema.name, &raced.id).await.unwrap();
    let candidate = InvitationPruneCandidate::from_entity(&row, cutoff).unwrap();
    store.mark_consumed(&raced.id, now).await.unwrap();
    assert_eq!(
        backend.prune_invitations(&[candidate]).await.unwrap(),
        0,
        "status compare must preserve an invitation accepted after candidate selection"
    );

    // Exercise pagination after deleting rows from the page being scanned.
    for index in 0..260 {
        let expires = if index % 2 == 0 { old } else { live };
        store
            .create(invitation(&format!("page-{index}"), expires))
            .await
            .unwrap();
    }
    assert_eq!(store.prune(cutoff).await.unwrap(), 132);
    for jti in ["old-expired", "old-consumed"] {
        assert!(store.find_by_jti(jti).await.unwrap().is_none());
    }
    for jti in [
        "live",
        "recently-consumed",
        "boundary",
        "malformed",
        "raced-acceptance",
        "page-259",
    ] {
        assert!(
            store.find_by_jti(jti).await.unwrap().is_some(),
            "{jti} must survive"
        );
    }
    assert!(backend.get(&schema.name, &boundary.id).await.is_ok());
    assert_eq!(store.prune(cutoff).await.unwrap(), 0, "sweep is idempotent");
    let invitation = store.find_by_jti("live").await.unwrap().unwrap();
    assert!(invitation.is_acceptable(now));
    store.mark_consumed(&invitation.id, now).await.unwrap();
    assert!(!store
        .find_by_jti("live")
        .await
        .unwrap()
        .unwrap()
        .is_acceptable(now));
}
