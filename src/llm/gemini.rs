use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const DEFAULT_MODEL: &str = "gemini-3.8-flash";

const BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Request<'a> {
    contents: [Content<'a>; 1],
    generation_config: GenerationConfig,
}

#[derive(Serialize)]
struct Content<'a> {
    parts: [Part<'a>; 1],
}

#[derive(Serialize)]
struct Part<'a> {
    text: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationConfig {
    response_mime_type: &'static str,
    temperature: f32,
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    candidates: Vec<Candidate>,
}

#[derive(Deserialize)]
struct Candidate {
    content: Option<CandidateContent>,
}

#[derive(Deserialize)]
struct CandidateContent {
    #[serde(default)]
    parts: Vec<CandidatePart>,
}

#[derive(Deserialize)]
struct CandidatePart {
    #[serde(default)]
    text: String,
}

/// Same contract as `ollama::generate_json`: the prompt in, the model's JSON
/// text out.
pub fn generate_json(prompt: &str, model: &str) -> Result<String> {
    let key = crate::api_key::gemini_key()
        .context("no Gemini key: add one from the stt-md menu or set GEMINI_API_KEY")?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()?;
    let body = Request {
        contents: [Content { parts: [Part { text: prompt }] }],
        generation_config: GenerationConfig {
            response_mime_type: "application/json",
            temperature: 0.2,
        },
    };
    let url = format!("{BASE_URL}/{model}:generateContent");
    let resp = client
        .post(&url)
        .header("x-goog-api-key", key)
        .json(&body)
        .send()
        .with_context(|| format!("failed to POST {url}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let txt = resp.text().unwrap_or_default();
        anyhow::bail!("gemini returned {status}: {txt}");
    }
    let parsed: Response = resp.json()?;
    let text: String = parsed
        .candidates
        .into_iter()
        .next()
        .and_then(|c| c.content)
        .map(|c| c.parts.into_iter().map(|p| p.text).collect())
        .unwrap_or_default();
    anyhow::ensure!(!text.is_empty(), "gemini returned no text");
    Ok(text)
}
