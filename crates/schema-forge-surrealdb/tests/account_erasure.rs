#![cfg(feature = "test-support")]
#[path = "../../schema-forge-backend/tests/support/account_erasure.rs"]
mod contract;
use schema_forge_backend::EntityStore;

#[tokio::test]
async fn account_erasure_removes_dependants_and_preserves_other_users() {
    let backend = schema_forge_surrealdb::test_support::connect("erasure", "complete")
        .await
        .unwrap();
    let fixture = contract::setup(&backend).await;
    // A DELETE event rejects the final step after the dependent deletes ran.
    backend.client().query("DEFINE EVENT reject_account_delete ON TABLE User WHEN $event = 'DELETE' THEN { THROW 'account erasure blocked'; };").await.unwrap().check().unwrap();
    assert!(backend.erase_account(&fixture.user.id).await.is_err());
    contract::assert_preserved(&backend, &fixture).await;
    backend
        .client()
        .query("REMOVE EVENT reject_account_delete ON TABLE User;")
        .await
        .unwrap()
        .check()
        .unwrap();
    contract::erase(&backend, &fixture).await;
}
