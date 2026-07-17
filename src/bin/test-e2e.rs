use anyhow::{Context, Result};
use chrono::Local;
use std::path::PathBuf;
use std::time::Instant;

use stt_md::{audio_utils, config::Config, llm, transcription::whisper::WhisperEngine, vault};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: test-e2e <mic.wav> [sys.wav] <vault-root>");
        std::process::exit(1);
    }
    let wav_path = PathBuf::from(&args[1]);
    let (sys_wav_path, vault_root) = if args.len() >= 4 {
        (Some(PathBuf::from(&args[2])), PathBuf::from(&args[3]))
    } else {
        (None, PathBuf::from(&args[2]))
    };

    println!("=== test-e2e ===");
    println!("mic wav: {}", wav_path.display());
    if let Some(p) = &sys_wav_path {
        println!("sys wav: {}", p.display());
    }
    println!("vault:   {}", vault_root.display());
    println!();

    let load = |path: &PathBuf| -> Result<Vec<f32>> {
        let t0 = Instant::now();
        let (mono, sr) = audio_utils::load_wav_mono_f32(path)
            .with_context(|| format!("failed to load {}", path.display()))?;
        let resampled = audio_utils::resample_to_16k(&mono, sr);
        println!(
            "[1/5] loaded {:.1}s from {} in {}ms",
            resampled.len() as f32 / 16_000.0,
            path.display(),
            t0.elapsed().as_millis()
        );
        Ok(resampled)
    };
    let mic_samples = load(&wav_path)?;
    let sys_samples = sys_wav_path.as_ref().map(load).transpose()?;

    let cfg = Config::load_or_init()?;
    let t0 = Instant::now();
    let engine = WhisperEngine::load(&cfg.whisper_model_path())?;
    println!("[2/5] loaded whisper model in {}ms", t0.elapsed().as_millis());

    let transcribe = |samples: &[f32], label: &str| -> Result<Vec<stt_md::transcription::TranscriptSegment>> {
        let t0 = Instant::now();
        let segs = engine.transcribe(
            samples,
            &cfg.whisper_language,
            cfg.whisper_initial_prompt.as_deref(),
        )?;
        println!(
            "[2/5] transcribed {} ({} segments) in {}ms",
            label,
            segs.len(),
            t0.elapsed().as_millis()
        );
        Ok(segs)
    };

    let mic_segments = transcribe(&mic_samples, "mic")?;
    let segments = match &sys_samples {
        Some(sys) => {
            let sys_segments = transcribe(sys, "system")?;
            stt_md::transcription::merge_segments(mic_segments, sys_segments)
        }
        None => mic_segments,
    };
    for s in &segments {
        let mins = s.start_ms / 60_000;
        let secs = (s.start_ms % 60_000) / 1000;
        let label = s.speaker.map(|sp| format!("{}: ", sp.label())).unwrap_or_default();
        println!("      [{:02}:{:02}] {}{}", mins, secs, label, s.text);
    }

    let t0 = Instant::now();
    let vocab = vault::scanner::scan_vault(&vault_root)?;
    let areas = cfg
        .areas_dir
        .as_deref()
        .map(|d| vault::scanner::scan_areas(&vault_root, d))
        .unwrap_or_default();
    println!(
        "[3/5] scanned vault in {}ms ({} fm tags, {} inline tags, {} wikilinks, {} areas)",
        t0.elapsed().as_millis(),
        vocab.frontmatter_tags.len(),
        vocab.inline_tags.len(),
        vocab.wikilink_targets.len(),
        areas.len()
    );

    let transcript_text: String = segments
        .iter()
        .map(|s| {
            let mins = s.start_ms / 60_000;
            let secs = (s.start_ms % 60_000) / 1000;
            match s.speaker {
                Some(sp) => format!("[{:02}:{:02}] {}: {}\n", mins, secs, sp.label(), s.text),
                None => format!("[{:02}:{:02}] {}\n", mins, secs, s.text),
            }
        })
        .collect();

    let prompt = llm::prompts::build_summary_prompt(&transcript_text, &vocab, &areas);
    println!("[4/5] calling Ollama...");
    let t0 = Instant::now();
    let raw = llm::ollama::generate_json(
        &prompt,
        llm::ollama::DEFAULT_MODEL,
        llm::ollama::DEFAULT_URL,
    )?;
    println!("[4/5] ollama returned in {}ms", t0.elapsed().as_millis());

    let mut summary: llm::MeetingSummary = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("invalid JSON: {e}\nraw:\n{raw}"))?;
    summary.enforce_vocab(&vocab);
    summary.enforce_area(&areas);
    println!("[4/5] summary: {}", summary.title);
    println!("      tags: {:?}", summary.tags);
    println!("      project: {:?}", summary.project_wikilink);
    println!("      area: {:?}", summary.area);
    println!("      people: {:?}", summary.people);
    println!("      decisions: {}", summary.decisions.len());
    println!("      action_items: {}", summary.action_items.len());

    let started_at = Local::now();
    let longest_track = mic_samples
        .len()
        .max(sys_samples.as_ref().map_or(0, |s| s.len()));
    let duration_min = (longest_track as i64 / 16_000 / 60).max(1);

    let meetings_dir_rel = vault::expand_pattern(&cfg.meetings_dir, &started_at);
    let daily_note_rel = vault::expand_pattern(&cfg.daily_note, &started_at);
    let daily_template = cfg
        .daily_template
        .as_deref()
        .and_then(|rel| std::fs::read_to_string(vault_root.join(rel)).ok());

    let written = vault::meeting_writer::write_meeting(
        &vault_root,
        &meetings_dir_rel,
        started_at,
        &summary,
        &segments,
        duration_min,
        &wav_path,
        sys_wav_path.as_deref(),
    )?;
    println!("[5/5] wrote meeting → {}", written.absolute_path.display());

    let daily_path = vault::daily_appender::append_meeting_link(
        &vault_root,
        &daily_note_rel,
        daily_template.as_deref(),
        started_at,
        &summary.title,
        &written.stem,
        duration_min,
    )?;
    println!("[5/5] updated daily   → {}", daily_path.display());

    println!("\n✓ E2E test complete");
    Ok(())
}
