// Re-export TemplateEngine from the shared template_engine module.
// This preserves backward compatibility for any code referencing
// `crate::cloud::overrides::TemplateEngine`.
pub use crate::template_engine::{embedded_template, TemplateEngine};
