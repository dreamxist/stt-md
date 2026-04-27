use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::paths;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OutputMode {
    /// Obsidian-flavored: scans vault for tags + wikilinks, writes to
    /// `<vault>/2-calendar/YYYY/MM/meetings/`, appends link to today's daily.
    #[default]
    Obsidian,
    /// Plain-Markdown mode: no vault scan, no wikilinks, no daily appender.
    /// Just dumps the summary `.md` into `output_dir`.
    Simple,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub output_mode: OutputMode,

    /// Used when `output_mode = "obsidian"`. Root of the Obsidian vault.
    pub vault_root: PathBuf,

    /// Used when `output_mode = "simple"`. Flat directory where `.md` files land.
    #[serde(default = "default_output_dir")]
    pub output_dir: PathBuf,

    pub ollama_model: String,
    pub ollama_url: String,
    pub whisper_language: String,
    pub whisper_model_filename: String,
}

fn default_output_dir() -> PathBuf {
    dirs::document_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("stt-md-notes")
}

impl Default for Config {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        Self {
            // New installs default to `simple` so the app works without Obsidian.
            // Existing config files without an `output_mode` field fall back to
            // `obsidian` via OutputMode::default() (backward-compat).
            output_mode: OutputMode::Simple,
            vault_root: home.join("Documents").join("Obsidian").join("vault"),
            output_dir: default_output_dir(),
            ollama_model: "qwen2.5:7b".to_string(),
            ollama_url: "http://localhost:11434".to_string(),
            whisper_language: "es".to_string(),
            whisper_model_filename: "ggml-large-v3-turbo.bin".to_string(),
        }
    }
}

impl Config {
    pub fn config_path() -> PathBuf {
        paths::app_support_dir().join("config.toml")
    }

    /// Load from disk if present; otherwise create the default file and return it.
    pub fn load_or_init() -> Result<Self> {
        let path = Self::config_path();
        if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            let cfg: Config = toml::from_str(&text)?;
            return Ok(cfg);
        }
        let cfg = Config::default();
        cfg.write_to(&path)?;
        Ok(cfg)
    }

    pub fn write_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    pub fn whisper_model_path(&self) -> PathBuf {
        paths::models_dir().join(&self.whisper_model_filename)
    }
}
