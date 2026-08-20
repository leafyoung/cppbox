//! User settings persisted as ~/.cppbox/cppbox.yaml (user home directory).
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsFile {
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_font_size")]
    pub font_size: u32,
    #[serde(default = "default_indent")]
    pub indent: u32,
    #[serde(default = "default_std")]
    pub std: String,
}

fn default_theme() -> String { "material-ocean".into() }
fn default_font_size() -> u32 { 14 }
fn default_indent() -> u32 { 2 }
fn default_std() -> String { "c++17".into() }

impl Default for SettingsFile {
    fn default() -> Self {
        SettingsFile {
            theme: default_theme(),
            font_size: default_font_size(),
            indent: default_indent(),
            std: default_std(),
        }
    }
}

/// `~/.cppbox/cppbox.yaml`
pub fn settings_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".cppbox")
        .join("cppbox.yaml")
}

pub fn load() -> SettingsFile {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|s| serde_yaml::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(s: &SettingsFile) -> std::io::Result<PathBuf> {
    let p = settings_path();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&p, serde_yaml::to_string(s).unwrap_or_default())?;
    Ok(p)
}
