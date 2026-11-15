//! From a finished recording to a note on disk.
//!
//! One function for every caller — the menubar's job queue, `--selftest` and
//! the `test-e2e` tool — so what gets exercised from a terminal is what runs
//! after a real meeting.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use chrono::{DateTime, Local};

use crate::config::{Config, OutputMode};
use crate::transcription::whisper::{self, WhisperEngine};
use crate::transcription::{self, TranscriptSegment};
use crate::{audio_utils, llm, vault};

const SAMPLE_RATE: usize = audio_utils::TARGET_RATE as usize;

/// A recording to turn into a note.
#[derive(Debug, Clone)]
pub struct RecordingInput {
    pub mic_path: PathBuf,
    pub sys_path: Option<PathBuf>,
    pub started_at: DateTime<Local>,
    /// Wall-clock length of the meeting. `None` when nobody was there to
    /// measure it (a recording recovered after a crash): the audio decides.
    pub duration_min: Option<i64>,
    /// Calendar event that was on when the recording started.
    pub calendar_title: Option<String>,
}

/// Where the pipeline is, for whoever is showing progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    Loading,
    Transcribing { percent: u8 },
    Summarizing,
    Writing,
}

impl Stage {
    /// Short Spanish label for the menu and the tooltip.
    pub fn label(&self) -> String {
        match self {
            Stage::Loading => "cargando audio".to_string(),
            Stage::Transcribing { percent } => format!("transcribiendo {percent}%"),
            Stage::Summarizing => "resumiendo".to_string(),
            Stage::Writing => "escribiendo la nota".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProcessedMeeting {
    pub note_path: PathBuf,
    pub title: String,
    pub duration_min: i64,
    /// Which model wrote the summary; `None` when every one of them failed and
    /// the note carries the transcript alone.
    pub model: Option<String>,
    pub segments: usize,
    /// The note was written but its link could not be added to the daily.
    pub daily_error: Option<String>,
}

/// The recording holds nothing a note could be made of: too short, or no
/// voice in it. Not a failure to retry — there is nothing to get right.
#[derive(Debug)]
pub struct NothingToTranscribe(pub String);

impl std::fmt::Display for NothingToTranscribe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NothingToTranscribe {}

pub fn process(
    input: &RecordingInput,
    cfg: &Config,
    on_stage: &mut dyn FnMut(Stage),
) -> Result<ProcessedMeeting> {
    on_stage(Stage::Loading);
    let mic_samples = load_track(&input.mic_path)?;
    // The other side's track is worth a lot, but not the whole meeting: if it
    // can't be read, the note is made from the mic and says so.
    let mut sys_unreadable = None;
    let sys_samples = input.sys_path.as_deref().and_then(|path| {
        load_track(path)
            .map_err(|e| {
                eprintln!("[stt-md] system track unreadable, using the mic alone: {e:#}");
                sys_unreadable = Some(e.to_string());
            })
            .ok()
    });

    // A track shorter than 1 s has nothing transcribable (e.g. system audio of
    // a meeting where nobody else spoke). Skip it; only give up if every
    // track is empty.
    let mic_ok = mic_samples.len() >= SAMPLE_RATE;
    let sys = sys_samples.as_deref().filter(|s| s.len() >= SAMPLE_RATE);
    if !mic_ok && sys.is_none() {
        return Err(NothingToTranscribe(format!(
            "la grabación dura menos de 1 segundo (audio: {})",
            input.mic_path.display()
        ))
        .into());
    }

    let segments = transcribe_tracks(cfg, mic_ok.then_some(&mic_samples[..]), sys, on_stage)?;
    if segments.is_empty() {
        return Err(NothingToTranscribe(format!(
            "no se detectó voz en la grabación (audio: {})",
            input.mic_path.display()
        ))
        .into());
    }

    // A tap that died mid-meeting leaves the note looking like a monologue.
    // Say it out loud — in the note and to the summarizer — instead of letting
    // a model fill the gap with people and deadlines nobody mentioned.
    let audio_warning = sys_samples
        .as_deref()
        .and_then(|s| {
            let (at, run) = audio_utils::dead_air(s, audio_utils::TARGET_RATE)?;
            println!("[stt-md] system track flatlined at {at:.0}s for {run:.0}s");
            Some(dead_air_warning(at, run, s.len() as f32 / SAMPLE_RATE as f32))
        })
        .or_else(|| {
            sys_unreadable.map(|e| {
                format!("No se pudo leer la pista del sistema ({e}): esta nota sale solo del micrófono.")
            })
        });

    let mut transcript_text: String = segments.iter().map(transcript_line).collect();
    if let Some(warning) = &audio_warning {
        transcript_text.insert_str(0, &format!("[aviso del grabador: {warning}]\n\n"));
    }

    let longest = mic_samples.len().max(sys_samples.as_ref().map_or(0, Vec::len));
    let duration_min = input
        .duration_min
        .unwrap_or((longest / SAMPLE_RATE / 60) as i64)
        .max(1);

    on_stage(Stage::Summarizing);
    let calendar_title = input.calendar_title.as_deref();
    let models = cfg.summary_models();
    let model_refs: Vec<&str> = models.iter().map(String::as_str).collect();
    println!("[stt-md] summarizing with {}", models.join(" → "));
    let t0 = Instant::now();

    match cfg.output_mode {
        OutputMode::Obsidian => {
            let vault_root = cfg.vault_root.as_path();
            let vocab = vault::scanner::scan_vault(vault_root)?;
            let areas = cfg
                .areas_dir
                .as_deref()
                .map(|d| vault::scanner::scan_areas(vault_root, d))
                .unwrap_or_default();
            println!(
                "[stt-md] scanned vault ({} tags / {} wikilinks / {} areas)",
                vocab.frontmatter_tags.len() + vocab.inline_tags.len(),
                vocab.wikilink_targets.len(),
                areas.len()
            );

            let prompt =
                llm::prompts::build_summary_prompt(&transcript_text, &vocab, &areas, calendar_title);
            // The transcript must survive a bad summary: a cloud model can be
            // out of reach and a 7B model returns broken JSON often enough.
            // Each model in the chain gets its turn, and if all fail the note
            // is written with the transcript and says why it has no summary.
            let mut summary = llm::summarize_with_fallback(&prompt, &model_refs, &cfg.ollama_url);
            println!("[stt-md] summary by {:?} in {}ms", summary.model, t0.elapsed().as_millis());
            summary.enforce_vocab(&vocab);
            summary.enforce_area(&areas);
            summary.enforce_grounding(&transcript_text);

            on_stage(Stage::Writing);
            let written = vault::meeting_writer::write_meeting(
                vault_root,
                &vault::expand_pattern(&cfg.meetings_dir, &input.started_at),
                input.started_at,
                &summary,
                &segments,
                duration_min,
                &vault::meeting_writer::NoteContext {
                    mic_path: &input.mic_path,
                    sys_path: input.sys_path.as_deref(),
                    warning: audio_warning.as_deref(),
                    event: calendar_title,
                },
            )?;
            println!("[stt-md] wrote meeting to {}", written.absolute_path.display());

            // The note exists from here on. Failing the whole job over the
            // daily would have a retry write the meeting a second time.
            let daily_template = cfg
                .daily_template
                .as_deref()
                .and_then(|rel| std::fs::read_to_string(vault_root.join(rel)).ok());
            let daily_error = vault::daily_appender::append_meeting_link(
                vault_root,
                &vault::expand_pattern(&cfg.daily_note, &input.started_at),
                daily_template.as_deref(),
                input.started_at,
                &summary.title,
                &written.stem,
                duration_min,
            )
            .map(|daily| println!("[stt-md] updated daily {}", daily.display()))
            .map_err(|e| {
                eprintln!("[stt-md] could not update the daily note: {e:#}");
                format!("{e:#}")
            })
            .err();

            Ok(ProcessedMeeting {
                note_path: written.absolute_path,
                title: summary.title,
                duration_min,
                model: summary.model,
                segments: segments.len(),
                daily_error,
            })
        }
        OutputMode::Simple => {
            let prompt = llm::prompts::build_simple_summary_prompt(&transcript_text, calendar_title);
            let mut summary = llm::summarize_with_fallback(&prompt, &model_refs, &cfg.ollama_url);
            println!("[stt-md] summary by {:?} in {}ms", summary.model, t0.elapsed().as_millis());
            summary.normalize_simple(4);
            summary.enforce_grounding(&transcript_text);

            on_stage(Stage::Writing);
            let path = vault::simple_writer::write_simple(
                cfg.output_dir.as_path(),
                input.started_at,
                &summary,
                &segments,
                duration_min,
                &input.mic_path,
                input.sys_path.as_deref(),
            )?;
            println!("[stt-md] wrote {}", path.display());
            Ok(ProcessedMeeting {
                note_path: path,
                title: summary.title,
                duration_min,
                model: summary.model,
                segments: segments.len(),
                daily_error: None,
            })
        }
    }
}

/// How the note words a stretch of total silence on the system track.
///
/// Silence to the very end is a tap that died: everything after it is missing.
/// Silence that ends is weaker evidence — a call sends exact zeros while
/// nobody on the other side talks — so the note says what is known and no
/// more. It used to claim the audio "never came back" for both, on meetings
/// where the other side spoke for another hour.
fn dead_air_warning(at_secs: f32, run_secs: f32, track_secs: f32) -> String {
    let clock = |secs: f32| format!("{:02}:{:02}", secs as u64 / 60, secs as u64 % 60);
    if at_secs + run_secs >= track_secs - 5.0 {
        format!(
            "El audio del sistema se cortó en {} y no volvió: desde ahí solo hay micrófono, la otra voz falta o aparece atribuida a «yo», y el resumen cubre nada más que la primera parte.",
            clock(at_secs)
        )
    } else {
        format!(
            "El audio del sistema estuvo en silencio total entre {} y {} ({} min). Puede que nadie más hablara; si alguien habló en ese tramo, no quedó grabado.",
            clock(at_secs),
            clock(at_secs + run_secs),
            (run_secs / 60.0).round().max(1.0) as u64
        )
    }
}

fn load_track(path: &Path) -> Result<Vec<f32>> {
    let t0 = Instant::now();
    let samples = audio_utils::load_wav_16k_mono(path)
        .map_err(|e| anyhow::anyhow!("no se pudo leer {}: {e}", path.display()))?;
    println!(
        "[stt-md] loaded {:.1}s from {} in {}ms",
        samples.len() as f32 / SAMPLE_RATE as f32,
        path.display(),
        t0.elapsed().as_millis()
    );
    Ok(samples)
}

/// Transcribes whichever tracks carry audio and reports progress across both
/// as one percentage.
fn transcribe_tracks(
    cfg: &Config,
    mic: Option<&[f32]>,
    sys: Option<&[f32]>,
    on_stage: &mut dyn FnMut(Stage),
) -> Result<Vec<TranscriptSegment>> {
    // Load per recording instead of caching: keeping large-v3-turbo resident
    // costs ~1.5 GB of RAM forever; reloading adds a few seconds.
    let model_path = cfg.whisper_model_path();
    let t0 = Instant::now();
    let engine = WhisperEngine::load(&model_path)?;
    println!(
        "[stt-md] whisper model {} loaded in {}ms",
        model_path.display(),
        t0.elapsed().as_millis()
    );

    let total: usize = [mic, sys]
        .into_iter()
        .flatten()
        .map(whisper::speech_window_count)
        .sum();
    let mut done_before = 0usize;
    let mut last_percent = None;
    on_stage(Stage::Transcribing { percent: 0 });

    let mut run = |samples: &[f32], label: &str| -> Result<Vec<TranscriptSegment>> {
        let t0 = Instant::now();
        let mut windows = 0;
        let segments = engine.transcribe_with_progress(
            samples,
            &cfg.whisper_language,
            cfg.whisper_initial_prompt.as_deref(),
            &mut |done, of| {
                windows = of;
                let percent = (((done_before + done) * 100) / total.max(1)).min(100) as u8;
                if last_percent != Some(percent) {
                    last_percent = Some(percent);
                    on_stage(Stage::Transcribing { percent });
                }
            },
        )?;
        done_before += windows;
        println!(
            "[stt-md] transcribed {label} ({} segments) in {}ms",
            segments.len(),
            t0.elapsed().as_millis()
        );
        Ok(segments)
    };

    let mic_segments = mic.map(|s| run(s, "mic")).transpose()?.unwrap_or_default();
    Ok(match sys {
        // Two tracks: transcribe each separately and interleave with speaker
        // labels — mixing them into one stream loses who spoke and clips.
        Some(sys) => transcription::merge_segments(mic_segments, run(sys, "system")?),
        // No system track, or an empty one: keep the mic unlabeled (labels
        // would be noise with a single voice source in the note).
        None => mic_segments,
    })
}

fn transcript_line(s: &TranscriptSegment) -> String {
    let mins = s.start_ms / 60_000;
    let secs = (s.start_ms % 60_000) / 1000;
    match s.speaker {
        Some(sp) => format!("[{:02}:{:02}] {}: {}\n", mins, secs, sp.label(), s.text),
        None => format!("[{:02}:{:02}] {}\n", mins, secs, s.text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcription::Speaker;

    #[test]
    fn transcript_lines_carry_the_speaker_when_there_is_one() {
        let mut s = TranscriptSegment {
            start_ms: 125_000,
            end_ms: 126_000,
            text: "hola".into(),
            speaker: Some(Speaker::Them),
        };
        assert_eq!(transcript_line(&s), "[02:05] ellos: hola\n");
        s.speaker = None;
        assert_eq!(transcript_line(&s), "[02:05] hola\n");
    }

    #[test]
    fn a_gap_that_ends_is_not_reported_as_audio_that_never_came_back() {
        // The real case: 278 s of zeros starting at 03:06 of a 63-minute call.
        let gap = dead_air_warning(186.0, 278.0, 3_817.0);
        assert!(gap.contains("entre 03:06 y 07:44 (5 min)"), "{gap}");
        assert!(!gap.contains("no volvió"));

        let dead = dead_air_warning(600.0, 900.0, 1_500.0);
        assert!(dead.contains("se cortó en 10:00 y no volvió"), "{dead}");
    }

    #[test]
    fn stage_labels_read_as_what_is_happening() {
        assert_eq!(Stage::Transcribing { percent: 42 }.label(), "transcribiendo 42%");
        assert_eq!(Stage::Summarizing.label(), "resumiendo");
    }

    #[test]
    fn a_recording_with_no_audio_is_nothing_to_transcribe_not_a_failure() {
        let dir = std::env::temp_dir().join(format!("stt-md-pipeline-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mic = dir.join("short-mic.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&mic, spec).unwrap();
        for _ in 0..4_000 {
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();

        let input = RecordingInput {
            mic_path: mic,
            sys_path: None,
            started_at: Local::now(),
            duration_min: None,
            calendar_title: None,
        };
        let err = process(&input, &Config::default(), &mut |_| {}).unwrap_err();
        assert!(err.downcast_ref::<NothingToTranscribe>().is_some(), "{err:#}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
