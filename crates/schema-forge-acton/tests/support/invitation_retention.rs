//! Shared retention contract on real storage, including concurrent acceptance.
use chrono::{DateTime, TimeDelta, Utc};
use schema_forge_backend::{
    invite_store::{InvitationPruneCandidate, FORGE_INVITATION_SCHEMA},
    EntityInviteStore, EntityStore, InvitationListQuery, InvitationTransition, InviteStatus,
    InviteStore, NewInvitation, SchemaBackend, TenantRef,
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
    lifecycle_contract(backend, &store, &schema.name, now).await;
}

async fn lifecycle_contract<B: EntityStore + 'static>(
    backend: Arc<B>,
    store: &EntityInviteStore,
    schema: &schema_forge_core::types::SchemaName,
    now: DateTime<Utc>,
) {
    let live = now + TimeDelta::days(7);
    let absent = schema_forge_core::types::EntityId::new("ForgeInvitation");
    assert!(store.find_by_id(&absent).await.unwrap().is_none());
    assert!(!store.try_consume(&absent, now).await.unwrap());
    assert!(!store.revoke(&absent, now).await.unwrap());
    assert!(store.find_by_id(&absent).await.unwrap().is_none());
    let revoked = store
        .create(invitation("lifecycle-revoked", live))
        .await
        .unwrap();
    assert!(revoked.created_at().is_some());
    assert!(store.revoke(&revoked.id, now).await.unwrap());
    assert!(!store.try_consume(&revoked.id, now).await.unwrap());
    assert!(store.mark_consumed(&revoked.id, now).await.is_err());
    assert!(!store.revoke(&revoked.id, now).await.unwrap());
    let row = store.find_by_id(&revoked.id).await.unwrap().unwrap();
    assert_eq!(row.status, InviteStatus::Revoked);
    assert!(row.consumed_at.is_none());

    let expired = store
        .create(invitation("lifecycle-expired", now))
        .await
        .unwrap();
    assert!(!store.try_consume(&expired.id, now).await.unwrap());
    assert_eq!(
        store.find_by_id(&expired.id).await.unwrap().unwrap().status,
        InviteStatus::Pending
    );
    let changed = store
        .create(invitation("lifecycle-expiry-changed", live))
        .await
        .unwrap();
    let stale_expiry = live.to_rfc3339();
    let mut row = backend.get(schema, &changed.id).await.unwrap();
    row.fields.insert(
        "expires_at".into(),
        DynamicValue::Text((live + TimeDelta::days(1)).to_rfc3339()),
    );
    backend.update(&row).await.unwrap();
    assert!(!backend
        .transition_invitation(&InvitationTransition {
            id: changed.id.clone(),
            status: InviteStatus::Consumed,
            at: now,
            expected_expires_at: Some(stale_expiry),
        })
        .await
        .unwrap());
    assert!(backend
        .transition_invitation(&InvitationTransition {
            id: changed.id.clone(),
            status: InviteStatus::Pending,
            at: now,
            expected_expires_at: None,
        })
        .await
        .is_err());
    assert!(store.try_consume(&changed.id, now).await.unwrap());
    assert!(!store.revoke(&changed.id, now).await.unwrap());
    assert!(!store.try_consume(&changed.id, now).await.unwrap());
    assert!(store
        .mark_consumed(&changed.id, now + TimeDelta::days(1))
        .await
        .is_err());
    let consumed = store.find_by_id(&changed.id).await.unwrap().unwrap();
    assert_eq!(consumed.consumed_at, Some(now));
    assert_eq!(consumed.token, "v4.local.private-invite-material");

    for index in 0..8 {
        let raced = store
            .create(invitation(&format!("lifecycle-race-{index}"), live))
            .await
            .unwrap();
        let (consumed, revoked) = tokio::join!(
            store.try_consume(&raced.id, now),
            store.revoke(&raced.id, now)
        );
        let (consumed, revoked) = (consumed.unwrap(), revoked.unwrap());
        assert_ne!(
            consumed, revoked,
            "exactly one terminal transition must win"
        );
        let row = store.find_by_id(&raced.id).await.unwrap().unwrap();
        assert_eq!(
            row.status,
            if consumed {
                InviteStatus::Consumed
            } else {
                InviteStatus::Revoked
            }
        );
        assert_eq!(row.consumed_at.is_some(), consumed);
        assert!(!store.try_consume(&raced.id, now).await.unwrap());
    }

    let tenant = TenantRef {
        schema: "Organization".into(),
        entity_id: "org-list-contract".into(),
    };
    let mut expected = std::collections::BTreeSet::new();
    for index in 0..255 {
        let mut invite = invitation(
            &format!("lifecycle-list-{index}"),
            if index % 128 == 0 { live } else { now },
        );
        invite.tenant_type = Some(tenant.schema.clone());
        invite.tenant_id = Some(tenant.entity_id.clone());
        let row = store.create(invite).await.unwrap();
        if index % 128 == 0 {
            expected.insert(row.id.to_string());
        }
    }
    for (kind, id) in [
        ("Department", tenant.entity_id.as_str()),
        ("Organization", "org-foreign"),
    ] {
        let mut invite = invitation(&format!("foreign-{kind}-{id}"), live);
        invite.tenant_type = Some(kind.into());
        invite.tenant_id = Some(id.into());
        store.create(invite).await.unwrap();
    }
    let mut malformed = invitation("lifecycle-list-malformed", live);
    malformed.tenant_type = Some(tenant.schema.clone());
    malformed.tenant_id = Some(tenant.entity_id.clone());
    let malformed = store.create(malformed).await.unwrap();
    let mut row = backend.get(schema, &malformed.id).await.unwrap();
    row.fields
        .insert("expires_at".into(), DynamicValue::Text("not-a-date".into()));
    backend.update(&row).await.unwrap();
    assert!(!store.try_consume(&malformed.id, now).await.unwrap());
    let mut request = InvitationListQuery {
        tenant: Some(tenant),
        limit: 1,
        offset: 0,
        at: now,
    };
    let mut listed = std::collections::BTreeSet::new();
    loop {
        let page = store.list_pending(&request).await.unwrap();
        assert!(page.invitations.len() <= 1);
        for row in page.invitations {
            assert!(
                listed.insert(row.id.to_string()),
                "continuations cannot repeat invitations"
            );
        }
        let Some(next) = page.next_offset else {
            break;
        };
        assert!(next > request.offset);
        request.offset = next;
    }
    assert_eq!(listed, expected);
    request.offset = 10_000;
    assert!(store
        .list_pending(&request)
        .await
        .unwrap()
        .invitations
        .is_empty());
    request.limit = 0;
    assert!(store.list_pending(&request).await.is_err());
}
