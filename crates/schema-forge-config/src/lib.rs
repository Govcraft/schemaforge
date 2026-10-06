//! Portable configuration shared by server and generation tooling.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Optional generated-site identity and SVG assets.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SiteBrandingConfig {
    pub name: Option<String>,
    /// Defaults to name; an empty string disables the suffix.
    pub title_suffix: Option<String>,
    pub logo: Option<PathBuf>,
    pub logo_on_dark: Option<PathBuf>,
    pub favicon: Option<PathBuf>,
}
