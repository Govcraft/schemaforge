use std::fs;

use crate::cli::TemplateExportArgs;
use crate::error::CliError;
use crate::output::OutputContext;

/// Metadata for an embedded cloud UI template.
struct TemplateMeta {
    name: &'static str,
    level: &'static str,
    path: &'static str,
}

/// Catalog of all embedded cloud UI templates, grouped by atomic design level.
const TEMPLATE_CATALOG: &[TemplateMeta] = &[
    // Atoms
    TemplateMeta {
        name: "field_display",
        level: "atom",
        path: "cloud/atoms/field_display.html",
    },
    TemplateMeta {
        name: "field_input",
        level: "atom",
        path: "cloud/atoms/field_input.html",
    },
    TemplateMeta {
        name: "composite",
        level: "atom",
        path: "cloud/atoms/composite.html",
    },
    // Molecules
    TemplateMeta {
        name: "dashboard_card",
        level: "molecule",
        path: "molecules/dashboard_card.html",
    },
    // Organisms
    TemplateMeta {
        name: "entity_list_body",
        level: "organism",
        path: "cloud/fragments/entity_list_body.html",
    },
    // Pages
    TemplateMeta {
        name: "base",
        level: "page",
        path: "cloud/base.html",
    },
    TemplateMeta {
        name: "login",
        level: "page",
        path: "cloud/login.html",
    },
    TemplateMeta {
        name: "dashboard",
        level: "page",
        path: "cloud/dashboard.html",
    },
    TemplateMeta {
        name: "entity_list",
        level: "page",
        path: "cloud/entity_list.html",
    },
    TemplateMeta {
        name: "entity_list_kanban",
        level: "page",
        path: "cloud/entity_list_kanban.html",
    },
    TemplateMeta {
        name: "entity_form",
        level: "page",
        path: "cloud/entity_form.html",
    },
    TemplateMeta {
        name: "entity_detail",
        level: "page",
        path: "cloud/entity_detail.html",
    },
];

/// Print all embedded templates grouped by atomic design level.
pub fn list(output: &OutputContext) {
    let levels = ["atom", "molecule", "organism", "page"];

    for level in levels {
        let entries: Vec<_> = TEMPLATE_CATALOG.iter().filter(|t| t.level == level).collect();
        if entries.is_empty() {
            continue;
        }
        let heading = match level {
            "atom" => "Atoms",
            "molecule" => "Molecules",
            "organism" => "Organisms",
            "page" => "Pages",
            _ => level,
        };
        output.status(&format!("{heading}:"));
        for entry in entries {
            output.status(&format!("  {:<22} {}", entry.name, entry.path));
        }
    }
}

/// Export embedded templates to the filesystem.
pub fn export(args: &TemplateExportArgs, output: &OutputContext) -> Result<(), CliError> {
    let output_dir = if let Some(ref dir) = args.output_dir {
        dir.clone()
    } else {
        dirs::config_dir()
            .map(|d| d.join("schema-forge/templates"))
            .ok_or_else(|| CliError::Server {
                message: "Could not determine config directory; use --output-dir".to_string(),
            })?
    };

    // Filter templates by name and/or level
    let templates: Vec<_> = TEMPLATE_CATALOG
        .iter()
        .filter(|t| {
            if let Some(ref name) = args.name {
                return t.name == name.as_str();
            }
            if let Some(ref level) = args.level {
                return t.level == level.as_str();
            }
            true
        })
        .collect();

    if templates.is_empty() {
        if let Some(ref name) = args.name {
            return Err(CliError::Server {
                message: format!("Unknown template: {name}. Run 'templates list' to see available templates."),
            });
        }
        if let Some(ref level) = args.level {
            return Err(CliError::Server {
                message: format!("No templates at level: {level}. Valid levels: atom, molecule, organism, page."),
            });
        }
    }

    let mut exported = 0;
    for tmpl in &templates {
        let content =
            schema_forge_acton::cloud::overrides::embedded_template(tmpl.path).ok_or_else(
                || CliError::Server {
                    message: format!("Embedded template not found: {}", tmpl.path),
                },
            )?;

        let dest = output_dir.join(tmpl.path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| CliError::Server {
                message: format!("Failed to create directory {}: {e}", parent.display()),
            })?;
        }
        fs::write(&dest, content).map_err(|e| CliError::Server {
            message: format!("Failed to write {}: {e}", dest.display()),
        })?;
        exported += 1;
    }

    output.success(&format!(
        "{exported} template(s) exported to {}",
        output_dir.display()
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_all_templates() {
        assert_eq!(TEMPLATE_CATALOG.len(), 12);
    }

    #[test]
    fn catalog_levels_are_valid() {
        for t in TEMPLATE_CATALOG {
            assert!(
                matches!(t.level, "atom" | "molecule" | "organism" | "page"),
                "invalid level: {}",
                t.level
            );
        }
    }

    #[test]
    fn all_embedded_templates_resolve() {
        for t in TEMPLATE_CATALOG {
            assert!(
                schema_forge_acton::cloud::overrides::embedded_template(t.path).is_some(),
                "missing embedded template: {}",
                t.path
            );
        }
    }

    #[test]
    fn export_unknown_name_errors() {
        let args = TemplateExportArgs {
            name: Some("nonexistent".to_string()),
            level: None,
            output_dir: Some(std::env::temp_dir().join("forge_test_templates")),
        };
        let output = OutputContext {
            mode: crate::output::OutputMode::Plain,
            verbose: 0,
            quiet: false,
            use_color: false,
        };
        let result = export(&args, &output);
        assert!(result.is_err());
    }
}
