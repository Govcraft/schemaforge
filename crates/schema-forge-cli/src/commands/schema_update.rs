//! Plan metadata persistence separately from physical database changes.

use schema_forge_acton::DynSchemaBackend;
use schema_forge_core::{
    migration::{DiffEngine, MigrationPlan},
    types::SchemaDefinition,
};

use crate::error::CliError;

pub(super) struct SchemaUpdate {
    pub schema: SchemaDefinition,
    pub migration: MigrationPlan,
    pub metadata_changed: bool,
}

impl SchemaUpdate {
    /// Preserve stored identity when comparing freshly parsed definitions.
    pub fn plan(existing: Option<&SchemaDefinition>, desired: &SchemaDefinition) -> Self {
        let mut schema = desired.clone();
        if let Some(existing) = existing {
            schema.id = existing.id.clone();
        }
        let metadata_changed = existing != Some(&schema);
        let migration = existing.map_or_else(
            || DiffEngine::create_new(&schema),
            |existing| DiffEngine::diff(existing, &schema),
        );
        Self {
            schema,
            migration,
            metadata_changed,
        }
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
            _: &'a SchemaName,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Option<SchemaDefinition>, BackendError>>
                    + Send
                    + Sync
                    + 'a,
            >,
        > {
            Box::pin(async move { Ok(self.stored.lock().unwrap().schema.clone()) })
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
