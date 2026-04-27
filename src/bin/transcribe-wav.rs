use anyhow::{Context, Result};
use chrono::Local;
use std::path::PathBuf;
use std::time::Instant;

use stt_md::{audio_utils, paths, transcription::whisper::WhisperEngine, vault};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: transcribe-wav <path/to/file.wav> [title]");
        std::process::exit(1);
    }
    let wav_path = PathBuf::from(&args[1]);
    let title = args.get(2).cloned().unwrap_or_else(|| "test".to_string());

    let t0 = Instant::now();
    let (mono, sr) = audio_utils::load_wav_mono_f32(&wav_path)
        .with_context(|| format!("failed to load {}", wav_path.display()))?;
    let resampled = audio_utils::resample_to_16k(&mono, sr);
    println!(
        "loaded {:.1}s of audio @ {sr}Hz → 16kHz in {}ms",
        resampled.len() as f32 / 16_000.0,
        t0.elapsed().as_millis()
    );

    let model_path = paths::whisper_model_path();
    let t0 = Instant::now();
    let engine = WhisperEngine::load(&model_path)?;
    println!("loaded model in {}ms", t0.elapsed().as_millis());

    let t0 = Instant::now();
    let segments = engine.transcribe(&resampled)?;
    println!(
        "transcribed {} segments in {}ms",
        segments.len(),
        t0.elapsed().as_millis()
    );

    let md = vault::meeting_writer::write_basic_md(
        &paths::transcripts_dir(),
        &title,
        Local::now(),
        &segments,
        &wav_path,
    )?;
    println!("wrote {}", md.display());

    Ok(())
}
