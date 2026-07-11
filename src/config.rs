use anyhow::{Context, Result};
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
    /// Optional so a `simple`-mode config doesn't need to declare it.
    #[serde(default = "default_vault_root")]
    pub vault_root: PathBuf,

    /// Used when `output_mode = "simple"`. Flat directory where `.md` files land.
    #[serde(default = "default_output_dir")]
    pub output_dir: PathBuf,

    /// Obsidian mode: vault-relative folder where meeting notes land.
    /// Placeholders: {year}, {month}, {week} (ISO, 2 dígitos), {date} (YYYY-MM-DD).
    #[serde(default = "default_meetings_dir")]
    pub meetings_dir: String,

    /// Obsidian mode: vault-relative path of the daily note that gets the
    /// meeting link appended. Same placeholders as `meetings_dir`.
    #[serde(default = "default_daily_note")]
    pub daily_note: String,

    /// Obsidian mode: vault-relative folder whose subfolders are life/work
    /// areas (e.g. "areas" → areas/work/proyecto). When set, the LLM
    /// attributes each meeting to one area from that closed list and it's
    /// written as `area:` in the frontmatter. `None` disables it.
    #[serde(default)]
    pub areas_dir: Option<String>,

    /// Obsidian mode: vault-relative path to the daily-note template. When the
    /// daily doesn't exist yet, it's created from this template (supports
    /// `{{date:YYYY-MM-DD}}` and `{{date:dddd}}`) instead of a minimal stub.
    #[serde(default)]
    pub daily_template: Option<String>,

    pub ollama_model: String,
    pub ollama_url: String,
    pub whisper_language: String,
    pub whisper_model_filename: String,

    /// Optional domain vocabulary fed to Whisper as initial prompt (helps it
    /// spell project names / jargon correctly). `None` uses a generic default.
    #[serde(default)]
    pub whisper_initial_prompt: Option<String>,
}

fn default_output_dir() -> PathBuf {
    dirs::document_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("stt-md-notes")
}

fn default_meetings_dir() -> String {
    "2-calendar/{year}/{month}/meetings".to_string()
}

fn default_daily_note() -> String {
    "2-calendar/{year}/{month}/{date}.md".to_string()
}

fn default_vault_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join("Documents")
        .join("Obsidian")
        .join("vault")
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // New installs default to `simple` so the app works without Obsidian.
            // Existing config files without an `output_mode` field fall back to
            // `obsidian` via OutputMode::default() (backward-compat).
            output_mode: OutputMode::Simple,
            vault_root: default_vault_root(),
            output_dir: default_output_dir(),
            meetings_dir: default_meetings_dir(),
            daily_note: default_daily_note(),
            areas_dir: None,
            daily_template: None,
            ollama_model: "qwen2.5:7b".to_string(),
            ollama_url: "http://localhost:11434".to_string(),
            whisper_language: "es".to_string(),
            whisper_model_filename: "ggml-large-v3-turbo.bin".to_string(),
            whisper_initial_prompt: None,
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
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("no se pudo leer {}", path.display()))?;
            let cfg: Config = toml::from_str(&text)
                .with_context(|| format!("config inválida en {}", path.display()))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_mode_config_parses_without_vault_root() {
        // Exactly the README's simple-mode example: must not fail to parse.
        let cfg: Config = toml::from_str(
            r#"
output_mode = "simple"
output_dir = "/tmp/stt-md-notes"
ollama_model = "qwen2.5:7b"
ollama_url = "http://localhost:11434"
whisper_language = "es"
whisper_model_filename = "ggml-large-v3-turbo.bin"
"#,
        )
        .expect("simple config without vault_root must parse");
        assert_eq!(cfg.output_mode, OutputMode::Simple);
    }

    #[test]
    fn legacy_config_without_output_mode_defaults_to_obsidian() {
        let cfg: Config = toml::from_str(
            r#"
vault_root = "/tmp/vault"
ollama_model = "qwen2.5:7b"
ollama_url = "http://localhost:11434"
whisper_language = "es"
whisper_model_filename = "ggml-large-v3-turbo.bin"
"#,
        )
        .unwrap();
        assert_eq!(cfg.output_mode, OutputMode::Obsidian);
        assert!(cfg.whisper_initial_prompt.is_none());
    }

    #[test]
    fn default_config_roundtrips_through_toml() {
        let cfg = Config::default();
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.output_mode, cfg.output_mode);
        assert_eq!(back.whisper_language, cfg.whisper_language);
    }
}
