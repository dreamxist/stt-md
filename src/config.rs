use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::paths;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub vault_root: PathBuf,
    pub ollama_model: String,
    pub ollama_url: String,
    pub whisper_language: String,
    pub whisper_model_filename: String,
}

impl Default for Config {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        Self {
            vault_root: home.join("home").join("brain"),
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
