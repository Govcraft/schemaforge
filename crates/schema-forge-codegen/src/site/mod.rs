//! Generate React sites independently of the CLI and server runtime.
mod branding;
mod context;
mod mapping;
mod render;
mod vendor;
use self::context::{EntityView, PageContext, SchemaMeta, SiteContext};
use self::render::SiteRenderer;
use crate::codegen::{FilePlan, WriteMode};
use crate::error::GenerationError;
use heck::ToKebabCase;
use schema_forge_config::SiteBrandingConfig;
use schema_forge_core::types::SchemaDefinition;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Inputs for rendering a site. Paths are resolved only at the asset/template I/O boundaries.
#[derive(Debug, Clone, Default)]
pub struct SiteOptions {
    pub schema_dir: PathBuf,
    pub schema: Option<String>,
    pub name: Option<String>,
    pub title_suffix: Option<String>,
    pub logo: Option<PathBuf>,
    pub logo_on_dark: Option<PathBuf>,
    pub favicon: Option<PathBuf>,
    pub templates_dir: Option<PathBuf>,
    pub accessibility_contact: Option<String>,
    pub config_path: Option<PathBuf>,
}

/// Rendered file plan and nonfatal unsupported-field diagnostics.
pub struct SitePlan {
    pub files: Vec<FilePlan>,
    pub warnings: Vec<String>,
    pub entity_count: usize,
}

/// Render validated schemas into a file plan, without writing output files.
pub fn plan_site(
    schemas: &[SchemaDefinition],
    args: &SiteOptions,
    config: &SiteBrandingConfig,
) -> Result<SitePlan, GenerationError> {
    let targets = pick_target_schemas(schemas, args.schema.as_deref())?;
    let branding = branding::Branding::resolve(args, config, schemas)?;
    let slug: String = branding
        .name
        .to_kebab_case()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
        .collect();
    let slug = slug.trim_matches('-');
    let project_name = if slug.is_empty() {
        "application".to_owned()
    } else {
        slug.to_owned()
    };
    let catalog: BTreeMap<String, SchemaMeta> = schemas
        .iter()
        .map(|def| (def.name.as_str().to_string(), SchemaMeta::from_schema(def)))
        .collect();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    for def in targets {
        let entity = EntityView::from_schema(def, &catalog, &mut warnings)?;
        if entity.fields.is_empty() {
            warnings.push(format!(
                "site: skipping schema `{}`: no supported fields",
                def.name.as_str()
            ));
        } else {
            entities.push(entity);
        }
    }
    if entities.is_empty() {
        return Err(GenerationError::Config {
            message: format!("{}\nno schemas have any v0-supported fields: everything was skipped. v1 supports: Text, RichText, Integer, Float, Boolean, DateTime, Enum, Json, Relation(One|Many), Array(scalar|enum), Composite.", warnings.join("\n")),
        });
    }
    let ctx = SiteContext {
        branding,
        project_name,
        entities,
        accessibility_contact: args.accessibility_contact.clone(),
    };
    let templates_dir = args.templates_dir.clone().or_else(|| {
        let path = PathBuf::from("site-templates");
        path.is_dir().then_some(path)
    });
    let renderer = SiteRenderer::new(templates_dir)?;
    Ok(SitePlan {
        files: build_plan(&ctx, &renderer)?,
        warnings,
        entity_count: ctx.entities.len(),
    })
}

/// Choose which schemas to generate pages for.
///
/// - System schemas (`@system`) are always excluded — they are
///   control-plane tables, not user-facing data.
/// - If `wanted` is `Some(name)`, only that schema is returned (still
///   subject to the system-schema exclusion).
/// - Otherwise every non-system schema is returned, in DSL declaration order.
fn pick_target_schemas<'a>(
    schemas: &'a [SchemaDefinition],
    wanted: Option<&str>,
) -> Result<Vec<&'a SchemaDefinition>, GenerationError> {
    match wanted {
        Some(name) => {
            let found = schemas
                .iter()
                .find(|s| s.name.as_str() == name)
                .ok_or_else(|| GenerationError::Config {
                    message: format!(
                        "schema `{name}` not found. Available: {}",
                        schemas
                            .iter()
                            .map(|s| s.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                })?;
            if found.is_system() {
                return Err(GenerationError::Config {
                    message: format!(
                        "schema `{name}` is a @system schema; system schemas \
                         are excluded from the site generator."
                    ),
                });
            }
            Ok(vec![found])
        }
        None => {
            let all: Vec<&SchemaDefinition> = schemas.iter().filter(|s| !s.is_system()).collect();
            if all.is_empty() {
                return Err(GenerationError::Config {
                    message: "every schema in the directory is @system; \
                              nothing to generate."
                        .to_string(),
                });
            }
            Ok(all)
        }
    }
}

/// Build the flat [`FilePlan`] list describing every file the site generator
/// wants to produce. Pure function — no I/O beyond template rendering.
fn build_plan(
    ctx: &SiteContext,
    renderer: &SiteRenderer,
) -> Result<Vec<FilePlan>, GenerationError> {
    let mut plan: Vec<FilePlan> = Vec::with_capacity(32 + 3 * ctx.entities.len());

    // ---- Project-root user files (Preserve: scaffold once) ----
    //
    // package.json has no comment syntax, so we can't embed a `@generated`
    // marker to protect against user edits. Preserve mode scaffolds it
    // once and then lets the user run `pnpm add` freely without the
    // generator clobbering them on regen.
    plan.push(preserve(
        "package.json",
        renderer.render("package.json", ctx)?,
    ));
    plan.push(owned(
        "vite.config.ts",
        renderer.render("vite.config.ts", ctx)?,
    ));
    plan.push(owned("index.html", renderer.render("index.html", ctx)?));
    plan.push(owned(
        "tailwind.config.ts",
        renderer.render("tailwind.config.ts", ctx)?,
    ));
    plan.push(owned("tsconfig.json", vendor::TSCONFIG_JSON.to_string()));
    plan.push(owned(
        "tsconfig.node.json",
        vendor::TSCONFIG_NODE_JSON.to_string(),
    ));
    plan.push(owned(".gitignore", vendor::GITIGNORE.to_string()));
    plan.push(owned(
        "eslint.config.js",
        vendor::ESLINT_CONFIG_JS.to_string(),
    ));

    // Brand marks. Vite serves `public/` at the URL root, so the templates
    // can reference `/logo-mark-white.svg` and `/logo-mark.svg` directly
    // without bundler involvement. The white mark sits on the inked rail
    // and login left panel; the ink mark is the favicon.
    plan.push(owned(
        "public/logo-mark-white.svg",
        ctx.branding
            .logo_on_dark
            .clone()
            .map(Ok)
            .unwrap_or_else(|| renderer.render("public/logo-mark-white.svg", ctx))?,
    ));
    plan.push(owned(
        "public/logo-mark.svg",
        ctx.branding
            .logo
            .clone()
            .map(Ok)
            .unwrap_or_else(|| renderer.render("public/logo-mark.svg", ctx))?,
    ));

    plan.push(owned(
        "public/favicon.svg",
        ctx.branding
            .favicon
            .clone()
            .map(Ok)
            .unwrap_or_else(|| renderer.render("public/favicon.svg", ctx))?,
    ));

    // ---- src/ scaffolding ----
    plan.push(owned("src/main.tsx", renderer.render("src/main.tsx", ctx)?));
    plan.push(owned("src/App.tsx", renderer.render("src/App.tsx", ctx)?));
    plan.push(owned(
        "src/index.css",
        renderer.render("src/index.css", ctx)?,
    ));
    plan.push(owned(
        "src/lib/branding.ts",
        renderer.render("src/lib/branding.ts", ctx)?,
    ));
    plan.push(owned(
        "src/lib/utils.ts",
        vendor::SHADCN_UTILS_TS.to_string(),
    ));
    plan.push(owned(
        "src/lib/auth.ts",
        renderer.render("src/lib/auth.ts", ctx)?,
    ));
    plan.push(owned(
        "src/lib/require-auth.tsx",
        renderer.render("src/lib/require-auth.tsx", ctx)?,
    ));
    plan.push(owned(
        "src/lib/use-document-title.ts",
        renderer.render("src/lib/use-document-title.ts", ctx)?,
    ));

    plan.push(preserve(
        "src/lib/error-toast.ts",
        renderer.render("src/lib/error-toast.ts", ctx)?,
    ));

    // ---- shadcn primitives (vendored, owned, unmodified) ----
    plan.push(owned(
        "src/components/ui/button.tsx",
        vendor::SHADCN_BUTTON.to_string(),
    ));
    plan.push(owned(
        "src/components/ui/input.tsx",
        vendor::SHADCN_INPUT.to_string(),
    ));
    plan.push(owned(
        "src/components/ui/label.tsx",
        vendor::SHADCN_LABEL.to_string(),
    ));
    plan.push(owned(
        "src/components/ui/card.tsx",
        vendor::SHADCN_CARD.to_string(),
    ));
    plan.push(owned(
        "src/components/ui/form.tsx",
        vendor::SHADCN_FORM.to_string(),
    ));
    plan.push(owned(
        "src/components/ui/table.tsx",
        vendor::SHADCN_TABLE.to_string(),
    ));
    plan.push(owned(
        "src/components/ui/relation-select.tsx",
        vendor::RELATION_SELECT.to_string(),
    ));
    plan.push(owned(
        "src/components/ui/error-block.tsx",
        vendor::ERROR_BLOCK.to_string(),
    ));
    plan.push(owned(
        "src/components/ui/file-upload.tsx",
        vendor::FILE_UPLOAD.to_string(),
    ));
    plan.push(owned(
        "src/components/ui/confirm-dialog.tsx",
        vendor::CONFIRM_DIALOG.to_string(),
    ));

    // ---- Generated multi-entity code (shared across pages) ----
    plan.push(owned(
        "src/generated/api-client.ts",
        renderer.render("src/generated/api-client.ts", ctx)?,
    ));
    plan.push(owned(
        "src/generated/entity-types.ts",
        renderer.render("src/generated/entity-types.ts", ctx)?,
    ));
    plan.push(owned(
        "src/generated/zod-schemas.ts",
        renderer.render("src/generated/zod-schemas.ts", ctx)?,
    ));
    plan.push(owned(
        "src/generated/route-manifest.ts",
        renderer.render("src/generated/route-manifest.ts", ctx)?,
    ));
    plan.push(owned(
        "src/generated/formatters.ts",
        renderer.render("src/generated/formatters.ts", ctx)?,
    ));

    // ---- Top-level login page (Preserve: users restyle freely) ----
    //
    // Login is mounted at `/login`, outside the `/app` subtree, because the
    // app routes need to fall through to it on auth failure.
    plan.push(preserve(
        "src/pages/login.tsx",
        renderer.render("src/pages/login.tsx", ctx)?,
    ));

    for path in ["src/pages/invite.tsx", "src/pages/invite-accept.tsx"] {
        plan.push(preserve(path, renderer.render(path, ctx)?));
    }
    plan.push(owned(
        "src/generated/invites.ts",
        renderer.render("src/generated/invites.ts", ctx)?,
    ));

    // ---- Public accessibility statement (Owned: regenerated so policy
    // text stays current with the SchemaForge baseline). Required by
    // OMB M-24-08 §§II.A–II.D and 36 CFR 1194 §§603.2–603.3 to live on
    // every page, reachable in both authed and unauthed states. ----
    plan.push(owned(
        "src/pages/accessibility.tsx",
        renderer.render("src/pages/accessibility.tsx", ctx)?,
    ));

    // ---- `/app/*`: per-entity user-facing pages ----
    //
    // Lives under `src/app/pages/<kebab>/` so the path mirrors the route
    // tree (`/app/<kebab>`). Each page is split across two files:
    //
    //   * `<page>.generated.tsx` — Owned. Schema-driven data and helpers
    //     (column definitions, form-field rendering, detail rows, sort /
    //     filter whitelists, enum badge metadata). Always regenerated on
    //     every `site generate` run so schema edits flow through without
    //     the user having to do anything.
    //
    //   * `<page>.tsx`            — Preserve. A thin shell that imports
    //     the symbols from its `.generated` sibling and composes them
    //     into the final page. Users own these: restyle freely, drop in
    //     charts, add state, intercept the mutation — subsequent
    //     generator runs leave them alone unless `--force-user-files` is
    //     set.
    //
    // The split is the answer to issue #40: schema changes stop
    // clobbering user customizations, because the only bytes that need to
    // be rewritten live in the Owned sibling.
    for entity in &ctx.entities {
        let page_ctx = PageContext {
            project_name: ctx.project_name.clone(),
            entity: entity.clone(),
            accessibility_contact: ctx.accessibility_contact.clone(),
        };
        let page_dir = format!("src/app/pages/{}", entity.kebab);
        plan.push(owned(
            &format!("{page_dir}/list.generated.tsx"),
            renderer.render("src/app/pages/list.generated.tsx", &page_ctx)?,
        ));
        plan.push(preserve(
            &format!("{page_dir}/list.tsx"),
            renderer.render("src/app/pages/list.tsx", &page_ctx)?,
        ));
        plan.push(owned(
            &format!("{page_dir}/detail.generated.tsx"),
            renderer.render("src/app/pages/detail.generated.tsx", &page_ctx)?,
        ));
        plan.push(preserve(
            &format!("{page_dir}/detail.tsx"),
            renderer.render("src/app/pages/detail.tsx", &page_ctx)?,
        ));
        plan.push(owned(
            &format!("{page_dir}/edit.generated.tsx"),
            renderer.render("src/app/pages/edit.generated.tsx", &page_ctx)?,
        ));
        plan.push(preserve(
            &format!("{page_dir}/edit.tsx"),
            renderer.render("src/app/pages/edit.tsx", &page_ctx)?,
        ));
    }

    Ok(plan)
}

fn owned(path: &str, contents: String) -> FilePlan {
    FilePlan {
        relative_path: PathBuf::from(path),
        contents,
        mode: WriteMode::Owned,
    }
}

fn preserve(path: &str, contents: String) -> FilePlan {
    FilePlan {
        relative_path: PathBuf::from(path),
        contents,
        mode: WriteMode::Preserve,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        Annotation, EnumColor, EnumVariants, FieldAnnotation, FieldDefinition, FieldModifier,
        FieldName, FieldType, FileAccess, FileConstraints, IntegerConstraints, ListHint,
        MimePattern, SchemaId, SchemaName, TextConstraints,
    };
    use std::collections::BTreeMap;

    fn employee_schema() -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Employee").unwrap(),
            vec![
                FieldDefinition::with_modifiers(
                    FieldName::new("full_name").unwrap(),
                    FieldType::Text(TextConstraints::with_max_length(255)),
                    vec![FieldModifier::Required],
                ),
                FieldDefinition::new(
                    FieldName::new("age").unwrap(),
                    FieldType::Integer(IntegerConstraints::unconstrained()),
                ),
                FieldDefinition::new(FieldName::new("active").unwrap(), FieldType::Boolean),
            ],
            Vec::new(),
        )
        .unwrap()
    }

    #[test]
    fn pick_target_defaults_to_all_non_system() {
        let s = vec![employee_schema()];
        let t = pick_target_schemas(&s, None).unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].name.as_str(), "Employee");
    }

    #[test]
    fn pick_target_errors_on_unknown_name() {
        let s = vec![employee_schema()];
        let err = pick_target_schemas(&s, Some("Nope")).unwrap_err();
        assert!(matches!(err, GenerationError::Config { .. }));
    }

    fn opportunity_schema_with_enum_colors() -> SchemaDefinition {
        let mut colors = BTreeMap::new();
        colors.insert("won".to_string(), EnumColor::Green);
        colors.insert("lost".to_string(), EnumColor::Red);
        colors.insert("qualifying".to_string(), EnumColor::Neutral);
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Opportunity").unwrap(),
            vec![
                FieldDefinition::with_modifiers(
                    FieldName::new("title").unwrap(),
                    FieldType::Text(TextConstraints::with_max_length(255)),
                    vec![FieldModifier::Required],
                ),
                FieldDefinition::with_annotations(
                    FieldName::new("stage").unwrap(),
                    FieldType::Enum(
                        EnumVariants::new(vec!["qualifying".into(), "won".into(), "lost".into()])
                            .unwrap(),
                    ),
                    vec![FieldModifier::Required],
                    vec![FieldAnnotation::EnumColors { colors }],
                ),
            ],
            Vec::new(),
        )
        .unwrap()
    }

    #[test]
    fn list_template_emits_enum_colors_map() {
        use super::context::{EntityView, PageContext, SchemaMeta};
        use super::render::SiteRenderer;

        let schema = opportunity_schema_with_enum_colors();
        let mut catalog = BTreeMap::new();
        catalog.insert("Opportunity".to_string(), SchemaMeta::from_schema(&schema));
        let mut warnings = Vec::new();
        let entity = EntityView::from_schema(&schema, &catalog, &mut warnings).unwrap();
        let page_ctx = PageContext {
            project_name: "demo".to_string(),
            entity,
            accessibility_contact: None,
        };

        let renderer = SiteRenderer::new(None).unwrap();
        let rendered = renderer
            .render("src/app/pages/list.generated.tsx", &page_ctx)
            .expect("list.generated template must render");

        // Per-field color map emitted in declaration order.
        assert!(
            rendered.contains("\"stage\": {"),
            "ENUM_COLORS should carry `stage` key; got:\n{rendered}"
        );
        assert!(rendered.contains("\"won\": \"green\""));
        assert!(rendered.contains("\"lost\": \"red\""));
        assert!(rendered.contains("\"qualifying\": \"neutral\""));
        // Badge helper and classes table both present.
        assert!(rendered.contains("ENUM_BADGE_CLASSES"));
        assert!(rendered.contains("function EnumBadge("));
        // Enum column cell uses EnumBadge, not formatFieldValue.
        assert!(
            rendered.contains("<EnumBadge field=\"stage\""),
            "enum column must render via EnumBadge"
        );
    }

    fn schema_with_list_hints() -> SchemaDefinition {
        // Fields carry a mix of explicit hints and default behavior so we
        // can assert the full partition + auto-hide policy in one fixture.
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Opportunity").unwrap(),
            vec![
                FieldDefinition::with_modifiers(
                    FieldName::new("title").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                    vec![FieldModifier::Required],
                ),
                // Explicit column hint.
                FieldDefinition::with_annotations(
                    FieldName::new("stage").unwrap(),
                    FieldType::Enum(EnumVariants::new(vec!["new".into(), "won".into()]).unwrap()),
                    vec![FieldModifier::Required],
                    vec![FieldAnnotation::List {
                        hint: ListHint::Column,
                    }],
                ),
                // Rich text auto-hides by default.
                FieldDefinition::new(FieldName::new("description").unwrap(), FieldType::RichText),
                // Unannotated integer -> column.
                FieldDefinition::new(
                    FieldName::new("pwin").unwrap(),
                    FieldType::Integer(IntegerConstraints::unconstrained()),
                ),
                // Explicit hidden even though it would otherwise show.
                FieldDefinition::with_annotations(
                    FieldName::new("internal_flag").unwrap(),
                    FieldType::Boolean,
                    vec![],
                    vec![FieldAnnotation::List {
                        hint: ListHint::Hidden,
                    }],
                ),
            ],
            vec![Annotation::Display {
                field: FieldName::new("title").unwrap(),
            }],
        )
        .unwrap()
    }

    #[test]
    fn list_template_partitions_fields_by_list_placement() {
        use super::context::{EntityView, PageContext, SchemaMeta};
        use super::render::SiteRenderer;

        let schema = schema_with_list_hints();
        let mut catalog = BTreeMap::new();
        catalog.insert("Opportunity".to_string(), SchemaMeta::from_schema(&schema));
        let mut warnings = Vec::new();
        let entity = EntityView::from_schema(&schema, &catalog, &mut warnings).unwrap();

        // title had no explicit hint but is the @display field → promoted to primary.
        let title = entity.fields.iter().find(|f| f.leaf == "title").unwrap();
        assert_eq!(title.list_placement, "primary");
        // description is rich_text → auto-hidden.
        let description = entity
            .fields
            .iter()
            .find(|f| f.leaf == "description")
            .unwrap();
        assert_eq!(description.list_placement, "hidden");
        // pwin defaults to column.
        let pwin = entity.fields.iter().find(|f| f.leaf == "pwin").unwrap();
        assert_eq!(pwin.list_placement, "column");
        // internal_flag explicit hidden stays hidden.
        let flag = entity
            .fields
            .iter()
            .find(|f| f.leaf == "internal_flag")
            .unwrap();
        assert_eq!(flag.list_placement, "hidden");

        let page_ctx = PageContext {
            project_name: "demo".to_string(),
            entity,
            accessibility_contact: None,
        };
        let renderer = SiteRenderer::new(None).unwrap();
        let rendered = renderer
            .render("src/app/pages/list.generated.tsx", &page_ctx)
            .expect("list.generated template must render");

        // Primary cell renders as a distinctive link with font-semibold.
        assert!(
            rendered.contains("font-semibold text-foreground"),
            "primary cell must use distinctive styling"
        );
        assert!(
            rendered.contains("accessorKey: \"title\""),
            "primary field must appear as a column"
        );
        // Hidden fields must not appear at all.
        assert!(
            !rendered.contains("accessorKey: \"description\""),
            "rich_text field must auto-hide"
        );
        assert!(
            !rendered.contains("accessorKey: \"internal_flag\""),
            "explicit @list(hidden) must omit the field"
        );
        // SORTABLE_FIELDS excludes hidden fields.
        let sortable_block = rendered
            .split("SORTABLE_FIELDS: readonly string[] = [")
            .nth(1)
            .and_then(|s| s.split(']').next())
            .unwrap_or("");
        assert!(
            sortable_block.contains("\"title\""),
            "sortable block missing title:\n{sortable_block}"
        );
        assert!(sortable_block.contains("\"stage\""));
        assert!(sortable_block.contains("\"pwin\""));
        assert!(!sortable_block.contains("\"description\""));
        assert!(!sortable_block.contains("\"internal_flag\""));
    }

    /// Schema with a single optional `file` field used to drive the codegen
    /// branches added for issue #52.
    fn document_schema_with_file() -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Document").unwrap(),
            vec![
                FieldDefinition::with_modifiers(
                    FieldName::new("name").unwrap(),
                    FieldType::Text(TextConstraints::with_max_length(255)),
                    vec![FieldModifier::Required],
                ),
                FieldDefinition::new(
                    FieldName::new("attachment").unwrap(),
                    FieldType::File(FileConstraints {
                        bucket: "documents".into(),
                        max_size_bytes: 10 * 1024 * 1024,
                        mime_allowlist: vec![
                            MimePattern::Exact("application/pdf".into()),
                            MimePattern::Family("image".into()),
                        ],
                        access: FileAccess::Presigned,
                    }),
                ),
            ],
            Vec::new(),
        )
        .unwrap()
    }

    fn document_entity() -> super::context::EntityView {
        use super::context::{EntityView, SchemaMeta};
        let schema = document_schema_with_file();
        let mut catalog = BTreeMap::new();
        catalog.insert("Document".to_string(), SchemaMeta::from_schema(&schema));
        let mut warnings = Vec::new();
        EntityView::from_schema(&schema, &catalog, &mut warnings).unwrap()
    }

    #[test]
    fn edit_template_emits_file_upload_for_file_field() {
        use super::context::PageContext;
        use super::render::SiteRenderer;

        let entity = document_entity();
        // The flag is what gates the import + the FormFields prop signature.
        assert!(
            entity.has_file_field,
            "file field on Document must set has_file_field"
        );

        let page_ctx = PageContext {
            project_name: "demo".to_string(),
            entity,
            accessibility_contact: None,
        };
        let renderer = SiteRenderer::new(None).unwrap();
        let rendered = renderer
            .render("src/app/pages/edit.generated.tsx", &page_ctx)
            .expect("edit.generated must render");

        // `<FileUpload>` is wired up with the right schema, field name, and
        // file metadata. The earlier read-only stub is gone.
        assert!(
            rendered.contains("import { FileUpload, type FileAttachment }"),
            "edit.generated must import FileUpload when entity has file field"
        );
        assert!(
            rendered.contains("<FileUpload"),
            "edit.generated must instantiate <FileUpload> for file fields"
        );
        assert!(
            rendered.contains(r#"schema="Document""#),
            "FileUpload must receive the schema name"
        );
        assert!(
            rendered.contains(r#"fieldName="attachment""#),
            "FileUpload must receive the field name"
        );
        assert!(
            rendered.contains("entityId={entityId}"),
            "FileUpload must thread the entity id from FormFields prop"
        );
        assert!(
            rendered.contains(r#"access: "presigned""#),
            "FileUpload meta must carry the access mode"
        );
        assert!(
            rendered.contains("maxSizeBytes: 10485760"),
            "FileUpload meta must carry the byte limit"
        );
        assert!(
            rendered.contains("\"application/pdf\"") && rendered.contains("\"image/*\""),
            "FileUpload meta must carry the mime allowlist"
        );

        // FormFields now takes an `entityId?: string` prop.
        assert!(
            rendered.contains("entityId?: string"),
            "FormFields must accept entityId for file-bearing entities"
        );

        // Entity updates delegate to the shared normalizer, which excludes
        // file fields handled by the dedicated upload endpoints.
        assert!(rendered.contains("return normalizeFormPayload("));
        assert!(document_entity()
            .form_fields
            .iter()
            .any(|field| field.leaf == "attachment" && field.kind == "file"));
        let validators = include_str!("../../templates/site/src/generated/zod-schemas.ts.jinja");
        let payload_normalizer = validators
            .split("export function normalizeFormPayload(")
            .nth(1)
            .expect("shared payload normalizer must exist");
        assert!(payload_normalizer.contains(r#"field.kind === "file""#));
        assert!(payload_normalizer.contains("continue"));

        // The pre-fix stub must be gone.
        assert!(
            !rendered.contains("upload via API"),
            "old read-only stub message must be replaced by the upload widget"
        );
    }

    #[test]
    fn detail_template_emits_attachment_download_for_file_field() {
        use super::context::PageContext;
        use super::render::SiteRenderer;

        let entity = document_entity();
        let page_ctx = PageContext {
            project_name: "demo".to_string(),
            entity,
            accessibility_contact: None,
        };
        let renderer = SiteRenderer::new(None).unwrap();
        let rendered = renderer
            .render("src/app/pages/detail.generated.tsx", &page_ctx)
            .expect("detail.generated must render");

        assert!(
            rendered.contains("import { AttachmentDownload, type FileAttachment }"),
            "detail.generated must import AttachmentDownload"
        );
        assert!(
            rendered.contains("<AttachmentDownload"),
            "detail.generated must render <AttachmentDownload> for file fields"
        );
        assert!(
            rendered.contains(r#"schema="Document""#),
            "AttachmentDownload must receive schema name"
        );
        assert!(
            rendered.contains(r#"fieldName="attachment""#),
            "AttachmentDownload must receive field name"
        );
        assert!(
            rendered.contains(r#"access="presigned""#),
            "AttachmentDownload must receive access mode"
        );
    }

    #[test]
    fn list_template_hides_file_field_by_default() {
        use super::context::PageContext;
        use super::render::SiteRenderer;

        let entity = document_entity();
        // Sanity-check the placement before rendering so a regression in
        // `default_list_placement` fails this test loudly.
        let attachment = entity
            .fields
            .iter()
            .find(|f| f.leaf == "attachment")
            .expect("attachment field must be present");
        assert_eq!(
            attachment.list_placement, "hidden",
            "file fields must default to hidden in lists (issue #52)"
        );

        let page_ctx = PageContext {
            project_name: "demo".to_string(),
            entity,
            accessibility_contact: None,
        };
        let renderer = SiteRenderer::new(None).unwrap();
        let rendered = renderer
            .render("src/app/pages/list.generated.tsx", &page_ctx)
            .expect("list.generated must render");

        assert!(
            !rendered.contains("accessorKey: \"attachment\""),
            "file field must not appear as a list column by default"
        );
        let sortable_block = rendered
            .split("SORTABLE_FIELDS: readonly string[] = [")
            .nth(1)
            .and_then(|s| s.split(']').next())
            .unwrap_or("");
        assert!(
            !sortable_block.contains("\"attachment\""),
            "hidden file field must be excluded from SORTABLE_FIELDS"
        );
    }
}

#[cfg(test)]
mod plan_tests {
    use super::*;

    #[test]
    fn plan_renders_multiple_entities_and_keeps_custom_pages_user_owned() {
        let schemas = schema_forge_dsl::parse(
            "schema Invoice { number: text required } schema Department { name: text required }",
        )
        .expect("valid schemas");
        let plan = plan_site(
            &schemas,
            &SiteOptions {
                name: Some("Acme".into()),
                ..SiteOptions::default()
            },
            &SiteBrandingConfig::default(),
        )
        .expect("render site");
        assert_eq!(plan.entity_count, 2);
        assert!(plan.warnings.is_empty());
        assert!(plan.files.iter().any(|file| file.relative_path
            == std::path::Path::new("src/app/pages/invoice/edit.tsx")
            && file.mode == WriteMode::Preserve));
        assert!(plan.files.iter().any(|file| file.relative_path
            == std::path::Path::new("src/app/pages/department/edit.generated.tsx")
            && file.mode == WriteMode::Owned));
    }

    #[test]
    fn unknown_requested_schema_reports_available_names() {
        let schemas = schema_forge_dsl::parse("schema Invoice { number: text required }")
            .expect("valid schema");
        let error = plan_site(
            &schemas,
            &SiteOptions {
                schema: Some("Missing".into()),
                ..SiteOptions::default()
            },
            &SiteBrandingConfig::default(),
        )
        .err()
        .expect("unknown schema");
        assert!(error.to_string().contains("Available: Invoice"));
    }
}
