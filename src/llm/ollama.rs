use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const DEFAULT_URL: &str = "http://localhost:11434";
pub const DEFAULT_MODEL: &str = "qwen2.5:7b";

#[derive(Debug, Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    stream: bool,
    format: &'a str,
    options: OllamaOptions,
}

#[derive(Debug, Serialize)]
struct OllamaOptions {
    temperature: f32,
    num_ctx: u32,
}

#[derive(Debug, Deserialize)]
struct GenerateResponse {
    response: String,
}

pub fn generate_json(prompt: &str, model: &str, base_url: &str) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()?;

    let url = format!("{base_url}/api/generate");
    let body = GenerateRequest {
        model,
        prompt,
        stream: false,
        format: "json",
        options: OllamaOptions {
            temperature: 0.2,
            // A 30-min meeting transcribes to ~40 KB, and the prompt adds the
            // vault vocabulary on top. Past num_ctx Ollama truncates from the
            // front — dropping the rules and schema, which sit above the
            // transcript — and the model answers with an invented shape.
            num_ctx: 32_768,
        },
    };

    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .with_context(|| format!("failed to POST {url}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let txt = resp.text().unwrap_or_default();
        anyhow::bail!("ollama returned {status}: {txt}");
    }

    let parsed: GenerateResponse = resp.json()?;
    Ok(parsed.response)
}
