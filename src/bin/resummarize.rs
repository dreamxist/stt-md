use anyhow::{Context, Result};
use std::path::PathBuf;

use stt_md::{config::Config, llm, vault};

/// Re-runs only the summary of a meeting note, from the transcript already in
/// it: a minute against Ollama instead of re-transcribing the WAVs. Prints the
/// cleaned summary as JSON; writes nothing.
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let [_, note, vault_root, ..] = args.as_slice() else {
        eprintln!("usage: resummarize <meeting-note.md> <vault-root> [model]");
        std::process::exit(1);
    };
    let cfg = Config::load_or_init()?;
    let models: Vec<&str> = match args.get(3) {
        Some(m) => vec![m.as_str()],
        None => cfg.summary_models(),
    };
    let vault_root = PathBuf::from(vault_root);

    let content = std::fs::read_to_string(note).with_context(|| format!("reading {note}"))?;
    let transcript: String = content
        .lines()
        .filter(|l| l.starts_with('['))
        .map(|l| format!("{}\n", l.replace("**yo:**", "yo:").replace("**ellos:**", "ellos:")))
        .collect();

    let vocab = vault::scanner::scan_vault(&vault_root)?;
    let areas = cfg
        .areas_dir
        .as_deref()
        .map(|d| vault::scanner::scan_areas(&vault_root, d))
        .unwrap_or_default();
    let prompt = llm::prompts::build_summary_prompt(&transcript, &vocab, &areas);
    let mut summary = llm::summarize_with_fallback(&prompt, &models, &cfg.ollama_url);
    summary.enforce_vocab(&vocab);
    summary.enforce_area(&areas);
    summary.enforce_grounding(&transcript);
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}
