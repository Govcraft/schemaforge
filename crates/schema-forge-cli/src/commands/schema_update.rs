//! Plan metadata persistence separately from physical database changes.

use schema_forge_acton::DynSchemaBackend;
use schema_forge_core::{
    migration::{DiffEngine, MigrationPlan},
    types::SchemaDefinition,
};

use crate::error::CliError;

/// Construct the complete desired registry before performing any migration.
pub(super) fn merge_schema_definitions(
    existing: impl IntoIterator<Item = SchemaDefinition>,
    desired: &[SchemaDefinition],
) -> Vec<SchemaDefinition> {
    let mut registry: std::collections::BTreeMap<_, _> = existing
        .into_iter()
        .map(|schema| (schema.name.clone(), schema))
        .collect();
    registry.extend(
        desired
            .iter()
            .cloned()
            .map(|schema| (schema.name.clone(), schema)),
    );
    registry.into_values().collect()
}

pub(super) fn validate_tenant_hierarchy(proposed: &[SchemaDefinition]) -> Result<(), CliError> {
    schema_forge_backend::tenant::TenantConfig::from_schemas(proposed).map_err(|error| {
        CliError::Config {
            message: format!("invalid proposed tenant hierarchy: {error}"),
        }
    })?;
    Ok(())
}

pub(super) async fn preflight_schema_batch(
    backend: &dyn DynSchemaBackend,
    desired: &[SchemaDefinition],
) -> Result<(), CliError> {
    let existing = backend.list_schema_metadata().await?;
    validate_tenant_hierarchy(&merge_schema_definitions(existing, desired))
}

/// Refuse a known destructive batch before any schema or revision writes.
pub(super) fn preflight_destructive_batch(
    updates: &[SchemaUpdate],
    execute: bool,
    force: bool,
    interactive: bool,
) -> Result<(), CliError> {
    if execute && !force && !interactive {
        let destructive_steps: Vec<_> = updates
            .iter()
            .flat_map(|update| {
                update
                    .migration
                    .steps
                    .iter()
                    .filter(|step| {
                        step.safety() == schema_forge_core::migration::MigrationSafety::Destructive
                    })
                    .map(|step| format!("  {}: {step}", update.schema.name))
            })
            .collect();
        if !destructive_steps.is_empty() {
            return Err(CliError::RequiresForceBatch {
                details: destructive_steps.join("\n"),
            });
        }
    }
    Ok(())
}

pub(super) struct SchemaUpdate {
    pub schema: SchemaDefinition,
    pub migration: MigrationPlan,
    pub metadata_changed: bool,
}

impl SchemaUpdate {
    /// Preserve stored identity when comparing freshly parsed definitions.
    pub fn plan(
        existing: Option<&SchemaDefinition>,
        desired: &SchemaDefinition,
    ) -> Result<Self, CliError> {
        let mut schema = desired.clone();
        if let Some(existing) = existing {
            schema.id = existing.id.clone();
        }
        let metadata_changed = existing != Some(&schema);
        let migration = existing
            .map_or_else(
                || Ok(DiffEngine::create_new(&schema)),
                |existing| DiffEngine::plan_update(existing, &schema),
            )
            .map_err(|error| CliError::Config {
                message: error.to_string(),
            })?;
        Ok(Self {
            schema,
            migration,
            metadata_changed,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.migration.is_empty() && !self.metadata_changed
    }

    /// Never submit an empty migration merely to persist runtime metadata.
    pub async fn persist(&self, backend: &dyn DynSchemaBackend) -> Result<(), CliError> {
        if !self.migration.is_empty() {
            backend
                .apply_migration(&self.schema.name, &self.migration.steps)
                .await?;
        }
        if self.metadata_changed {
            backend.store_schema_metadata(&self.schema).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cli::{ApplyArgs, GlobalOpts, MigrateArgs},
        output::{OutputContext, OutputMode},
    };
    use schema_forge_backend::BackendError;
    use schema_forge_core::{migration::MigrationStep, types::SchemaName};
    use std::{future::Future, pin::Pin, sync::Mutex};

    #[derive(Default)]
    struct Stored {
        schema: Option<SchemaDefinition>,
        migrations: usize,
        writes: usize,
        preparations: usize,
    }

    #[derive(Default)]
    struct Backend {
        stored: Mutex<Stored>,
        fail_migration: bool,
        revisions_supported: bool,
    }

    impl Backend {
        fn seeded(schema: SchemaDefinition) -> Self {
            Self {
                stored: Mutex::new(Stored {
                    schema: Some(schema),
                    ..Stored::default()
                }),
                fail_migration: false,
                revisions_supported: false,
            }
        }
    }

    impl DynSchemaBackend for Backend {
        fn supports_record_revisions(&self) -> bool {
            self.revisions_supported
        }
        fn prepare_record_revisions<'a>(
            &'a self,
            _: &'a SchemaName,
        ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + Sync + 'a>> {
            Box::pin(async move {
                assert!(self.revisions_supported);
                self.stored.lock().unwrap().preparations += 1;
                Ok(())
            })
        }

        fn apply_migration<'a>(
            &'a self,
            _: &'a SchemaName,
            steps: &'a [MigrationStep],
        ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + Sync + 'a>> {
            Box::pin(async move {
                assert!(
                    !steps.is_empty(),
                    "metadata changes must not issue empty migrations"
                );
                if self.fail_migration {
                    return Err(BackendError::MigrationFailed {
                        step: "test".into(),
                        reason: "controlled failure".into(),
                    });
                }
                self.stored.lock().unwrap().migrations += 1;
                Ok(())
            })
        }
        fn store_schema_metadata<'a>(
            &'a self,
            schema: &'a SchemaDefinition,
        ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + Sync + 'a>> {
            Box::pin(async move {
                let mut stored = self.stored.lock().unwrap();
                stored.schema = Some(schema.clone());
                stored.writes += 1;
                Ok(())
            })
        }
        fn load_schema_metadata<'a>(
            &'a self,
            name: &'a SchemaName,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Option<SchemaDefinition>, BackendError>>
                    + Send
                    + Sync
                    + 'a,
            >,
        > {
            Box::pin(async move {
                Ok(self
                    .stored
                    .lock()
                    .unwrap()
                    .schema
                    .clone()
                    .filter(|schema| &schema.name == name))
            })
        }
        fn list_schema_metadata(
            &self,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Vec<SchemaDefinition>, BackendError>> + Send + Sync + '_,
            >,
        > {
            Box::pin(
                async move { Ok(self.stored.lock().unwrap().schema.iter().cloned().collect()) },
            )
        }
    }

    fn schema(source: &str) -> SchemaDefinition {
        schema_forge_dsl::parse(source).unwrap().remove(0)
    }

    fn output() -> OutputContext {
        OutputContext {
            mode: OutputMode::Human,
            verbose: 0,
            quiet: true,
            use_color: false,
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Command {
        Apply,
        Migrate,
    }

    impl Command {
        async fn run(
            self,
            backend: &Backend,
            desired: SchemaDefinition,
            execute: bool,
        ) -> Result<(), CliError> {
            match self {
                Self::Apply => {
                    super::super::apply::apply_to_backend(
                        &ApplyArgs {
                            paths: vec![],
                            dry_run: !execute,
                            force: false,
                            with_policies: false,
                            prepare_record_revisions: false,
                        },
                        &[desired],
                        backend,
                        &output(),
                    )
                    .await
                }
                Self::Migrate => {
                    super::super::migrate::migrate_on_backend(
                        &MigrateArgs {
                            paths: vec![],
                            execute,
                            force: false,
                            schema: None,
                        },
                        &[desired],
                        backend,
                        &output(),
                    )
                    .await
                }
            }
        }
    }

    #[tokio::test]
    async fn destructive_batch_is_refused_before_safe_schema_or_revision_writes() {
        let desired = [
            schema("schema Aaa { label: text }"),
            schema("schema Note { title: text }"),
        ];
        for command in [Command::Apply, Command::Migrate] {
            let mut backend = Backend::seeded(schema("schema Note { title: text extra: text }"));
            backend.revisions_supported = true;
            let result = match command {
                Command::Apply => {
                    super::super::apply::apply_to_backend(
                        &ApplyArgs {
                            paths: vec![],
                            dry_run: false,
                            force: false,
                            with_policies: false,
                            prepare_record_revisions: true,
                        },
                        &desired,
                        &backend,
                        &output(),
                    )
                    .await
                }
                Command::Migrate => {
                    super::super::migrate::migrate_on_backend(
                        &MigrateArgs {
                            paths: vec![],
                            execute: true,
                            force: false,
                            schema: None,
                        },
                        &desired,
                        &backend,
                        &output(),
                    )
                    .await
                }
            };
            assert!(
                matches!(result, Err(CliError::RequiresForceBatch { .. })),
                "{result:?}"
            );
            let stored = backend.stored.lock().unwrap();
            assert_eq!(stored.migrations, 0);
            assert_eq!(stored.writes, 0);
            assert_eq!(stored.preparations, 0);
        }
    }

    #[test]
    fn destructive_preflight_reports_every_schema_and_step() {
        let original = schema("schema Note { title: text extra: text other: text }");
        let task = schema("schema Task { title: text obsolete: text }");
        let updates = [
            SchemaUpdate::plan(Some(&original), &schema("schema Note { title: text }")).unwrap(),
            SchemaUpdate::plan(Some(&task), &schema("schema Task { title: text }")).unwrap(),
        ];
        let error = preflight_destructive_batch(&updates, true, false, false).unwrap_err();
        assert_eq!(
            error.exit_code() as i32,
            CliError::RequiresForce.exit_code() as i32
        );
        let message = error.to_string();
        for part in [
            "Note",
            "extra",
            "other",
            "Task",
            "obsolete",
            "no schemas were applied",
        ] {
            assert!(message.contains(part), "missing {part}: {message}");
        }
    }

    #[test]
    fn destructive_preflight_preserves_dry_run_force_and_interactive_modes() {
        let original = schema("schema Note { title: text extra: text }");
        let updates = [
            SchemaUpdate::plan(Some(&original), &schema("schema Note { title: text }")).unwrap(),
        ];
        for (execute, force, interactive) in [
            (false, false, false),
            (true, true, false),
            (true, false, true),
        ] {
            assert!(preflight_destructive_batch(&updates, execute, force, interactive).is_ok());
        }
        assert!(preflight_destructive_batch(&updates, true, false, false).is_err());
    }

    #[tokio::test]
    async fn tenancy_changes_never_write_even_with_force() {
        for force in [false, true] {
            for dry_run in [false, true] {
                let original = schema("schema Contact { phone: text unique }");
                let backend = Backend::seeded(original.clone());
                let result = super::super::apply::apply_to_backend(
                    &ApplyArgs {
                        paths: vec![],
                        force,
                        dry_run,
                        with_policies: false,
                        prepare_record_revisions: false,
                    },
                    &[schema(
                        r#"@tenant(parent: "Org") schema Contact { phone: text unique }"#,
                    )],
                    &backend,
                    &output(),
                )
                .await;
                assert!(matches!(result, Err(CliError::Config { .. })));
                let stored = backend.stored.lock().unwrap();
                assert_eq!(stored.migrations, 0);
                assert_eq!(stored.writes, 0);
                assert_eq!(stored.schema.as_ref(), Some(&original));
            }
        }
    }

    #[tokio::test]
    async fn invalid_combined_hierarchy_is_rejected_before_all_writes() {
        for command in [Command::Apply, Command::Migrate] {
            let original = schema("@tenant(root) schema Org { name: text }");
            for source in [
                "@tenant(root) schema Other { name: text }",
                r#"@tenant(parent: "Missing") schema Child { name: text }"#,
            ] {
                let backend = Backend::seeded(original.clone());
                assert!(command.run(&backend, schema(source), true).await.is_err());
                let stored = backend.stored.lock().unwrap();
                assert_eq!(stored.migrations, 0);
                assert_eq!(stored.writes, 0);
                assert_eq!(stored.schema.as_ref(), Some(&original));
            }
        }
    }

    #[test]
    fn proposed_hierarchy_replaces_old_definitions_and_accepts_valid_batches() {
        let old = schema("schema Contact { phone: text }");
        let desired = [
            schema("@tenant(root) schema Org { name: text }"),
            schema(r#"@tenant(parent: "Org") schema Contact { phone: text }"#),
        ];
        let proposed = merge_schema_definitions([old], &desired);
        assert_eq!(proposed.len(), 2);
        validate_tenant_hierarchy(&proposed).unwrap();
        assert!(validate_tenant_hierarchy(&[
            schema(r#"@tenant(parent: "Other") schema Org { name: text }"#),
            schema(r#"@tenant(parent: "Org") schema Other { name: text }"#)
        ])
        .is_err());
    }

    #[tokio::test]
    async fn lossy_transforms_require_force_before_any_writes() {
        for command in [Command::Apply, Command::Migrate] {
            for (old, new) in [
                (
                    "schema Sample { value: float }",
                    "schema Sample { value: integer }",
                ),
                (
                    "schema Sample { value: integer }",
                    "schema Sample { value: float }",
                ),
                (
                    r#"schema Sample { value: enum("old", "stay") }"#,
                    r#"schema Sample { value: enum("stay") }"#,
                ),
                (
                    "schema Sample { value: boolean }",
                    "schema Sample { value: datetime }",
                ),
            ] {
                let original = schema(old);
                let backend = Backend::seeded(original.clone());
                let result = command.run(&backend, schema(new), true).await;
                assert!(
                    matches!(result, Err(CliError::RequiresForceBatch { .. })),
                    "{command:?}: {result:?}"
                );
                let stored = backend.stored.lock().unwrap();
                assert_eq!(stored.migrations, 0);
                assert_eq!(stored.writes, 0);
                assert_eq!(stored.schema.as_ref(), Some(&original));
            }
        }
    }

    const ORIGINAL: &str = "@version(1) schema Person { age: integer }";
    const METADATA_CHANGES: [&str; 3] = [
        "@version(2) schema Person { age: integer }",
        "@version(1) @access(read: [\"reader\"]) schema Person { age: integer }",
        "@version(1) schema Person { age: integer @require(\"age >= 18\", \"must be adult\") }",
    ];

    #[tokio::test]
    async fn commands_persist_metadata_only_changes_and_preserve_identity() {
        for command in [Command::Apply, Command::Migrate] {
            for source in METADATA_CHANGES {
                let original = schema(ORIGINAL);
                let backend = Backend::seeded(original.clone());
                let mut desired = schema(source);
                command.run(&backend, desired.clone(), true).await.unwrap();
                desired.id = original.id;
                let stored = backend.stored.lock().unwrap();
                assert_eq!(
                    stored.schema.as_ref(),
                    Some(&desired),
                    "{command:?} {source}"
                );
                assert_eq!(stored.migrations, 0);
                assert_eq!(stored.writes, 1);
            }
        }
    }

    #[tokio::test]
    async fn repeated_parses_are_noops_after_metadata_update() {
        for command in [Command::Apply, Command::Migrate] {
            let original = schema(ORIGINAL);
            let backend = Backend::seeded(original.clone());
            command.run(&backend, schema(ORIGINAL), true).await.unwrap();
            assert_eq!(backend.stored.lock().unwrap().writes, 0);
            command
                .run(&backend, schema(METADATA_CHANGES[2]), true)
                .await
                .unwrap();
            command
                .run(&backend, schema(METADATA_CHANGES[2]), true)
                .await
                .unwrap();
            let stored = backend.stored.lock().unwrap();
            assert_eq!(stored.writes, 1);
            assert_eq!(stored.migrations, 0);
            assert_eq!(stored.schema.as_ref().unwrap().id, original.id);
        }
    }

    #[tokio::test]
    async fn dry_runs_never_write_metadata_or_ddl() {
        for command in [Command::Apply, Command::Migrate] {
            for source in METADATA_CHANGES {
                let original = schema(ORIGINAL);
                let backend = Backend::seeded(original.clone());
                command.run(&backend, schema(source), false).await.unwrap();
                let stored = backend.stored.lock().unwrap();
                assert_eq!(stored.schema.as_ref(), Some(&original));
                assert_eq!(stored.writes, 0);
                assert_eq!(stored.migrations, 0);
            }
        }
    }

    #[tokio::test]
    async fn physical_changes_still_migrate_before_persisting_metadata() {
        for command in [Command::Apply, Command::Migrate] {
            let backend = Backend::default();
            command.run(&backend, schema(ORIGINAL), true).await.unwrap();
            command
                .run(
                    &backend,
                    schema("@version(2) schema Person { age: integer name: text }"),
                    true,
                )
                .await
                .unwrap();
            let stored = backend.stored.lock().unwrap();
            assert_eq!(stored.migrations, 2);
            assert_eq!(stored.writes, 2);
            assert!(stored.schema.as_ref().unwrap().field("name").is_some());
        }
    }

    #[tokio::test]
    async fn failed_physical_migration_does_not_replace_metadata() {
        for command in [Command::Apply, Command::Migrate] {
            let original = schema(ORIGINAL);
            let mut backend = Backend::seeded(original.clone());
            backend.fail_migration = true;
            let result = command
                .run(
                    &backend,
                    schema("@version(2) schema Person { age: integer name: text }"),
                    true,
                )
                .await;
            assert!(matches!(
                result,
                Err(CliError::Backend(BackendError::MigrationFailed { .. }))
            ));
            let stored = backend.stored.lock().unwrap();
            assert_eq!(stored.schema.as_ref(), Some(&original));
            assert_eq!(stored.writes, 0);
        }
    }

    #[tokio::test]
    async fn invalid_rule_is_rejected_before_backend_connection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("person.schema");
        std::fs::write(
            &path,
            "@version(2) schema Person { age: integer @require(\"age\", \"must be adult\") }",
        )
        .unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "[schema_forge.signing]\nmode = \"off\"\n").unwrap();
        let global = GlobalOpts {
            config: Some(config),
            format: "human".into(),
            verbose: 0,
            quiet: true,
            no_color: true,
            db_url: Some("postgres://localhost:1/unreachable".into()),
            db_ns: None,
            db_name: None,
            trust_policy: None,
            no_verify: false,
        };
        let apply = super::super::apply::run(
            ApplyArgs {
                paths: vec![path.clone()],
                dry_run: false,
                force: false,
                with_policies: false,
                prepare_record_revisions: false,
            },
            &global,
            &output(),
        )
        .await;
        let migrate = super::super::migrate::run(
            MigrateArgs {
                paths: vec![path],
                execute: true,
                force: false,
                schema: None,
            },
            &global,
            &output(),
        )
        .await;
        for result in [apply, migrate] {
            assert!(
                matches!(result, Err(CliError::Parse { .. })),
                "must fail parsing before attempting connection: {result:?}"
            );
        }
    }
    #[tokio::test]
    async fn explicit_revision_preparation_handles_unchanged_dry_run_and_unsupported_schemas() {
        let desired = schema(ORIGINAL);
        let mut backend = Backend::seeded(desired.clone());
        let mut args = ApplyArgs {
            paths: vec![],
            dry_run: false,
            force: false,
            with_policies: false,
            prepare_record_revisions: true,
        };
        assert!(super::super::apply::apply_to_backend(
            &args,
            std::slice::from_ref(&desired),
            &backend,
            &output()
        )
        .await
        .is_err());
        assert_eq!(backend.stored.lock().unwrap().writes, 0);
        backend.revisions_supported = true;
        args.dry_run = true;
        super::super::apply::apply_to_backend(
            &args,
            std::slice::from_ref(&desired),
            &backend,
            &output(),
        )
        .await
        .unwrap();
        assert_eq!(backend.stored.lock().unwrap().preparations, 0);
        args.dry_run = false;
        for _ in 0..2 {
            super::super::apply::apply_to_backend(
                &args,
                std::slice::from_ref(&desired),
                &backend,
                &output(),
            )
            .await
            .unwrap();
        }
        let stored = backend.stored.lock().unwrap();
        assert_eq!(stored.preparations, 2);
        assert_eq!(stored.writes, 0);
        assert_eq!(stored.migrations, 0);
    }
}
