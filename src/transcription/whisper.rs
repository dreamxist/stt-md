use anyhow::{Context, Result};
use std::path::Path;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::TranscriptSegment;

const DEFAULT_INITIAL_PROMPT: &str =
    "Reunión técnica de software en español. Vocabulario común: \
     whisper, Ollama, Obsidian, vault, prompt, LLM, frontend, backend, \
     deploy, sprint, standup, PR, merge, commit.";

pub struct WhisperEngine {
    ctx: WhisperContext,
}

impl WhisperEngine {
    pub fn load(model_path: &Path) -> Result<Self> {
        let model_str = model_path
            .to_str()
            .context("model path is not valid UTF-8")?;
        let params = WhisperContextParameters::default();
        let ctx = WhisperContext::new_with_params(model_str, params)
            .with_context(|| format!("failed to load whisper model at {model_str}"))?;
        Ok(Self { ctx })
    }

    /// Transcribe 16kHz mono f32 samples. `language` is a whisper.cpp language
    /// code ("es", "en", "auto", …). `initial_prompt` biases spelling of domain
    /// vocabulary; `None` uses a generic Spanish default.
    ///
    /// Long recordings are cut at quiet points into windows of at most
    /// ~30 s and each window is decoded independently. Feeding whisper.cpp a
    /// full 90-minute buffer lets one bad decode (e.g. echoing the prompt)
    /// poison every later window through the carried-over context, and the
    /// rest of the meeting silently vanishes. Silent windows are skipped,
    /// which is also where whisper invents text.
    pub fn transcribe(
        &self,
        samples_16k_mono: &[f32],
        language: &str,
        initial_prompt: Option<&str>,
    ) -> Result<Vec<TranscriptSegment>> {
        let prompt = initial_prompt.unwrap_or(DEFAULT_INITIAL_PROMPT);
        let mut state = self.ctx.create_state()?;
        let mut segments = Vec::new();

        for (start, end) in speech_windows(samples_16k_mono) {
            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            params.set_language(Some(language));
            params.set_translate(false);
            params.set_no_context(true);
            params.set_print_special(false);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            params.set_n_threads(num_cpus_for_whisper());
            params.set_initial_prompt(prompt);

            state.full(params, &samples_16k_mono[start..end])?;

            let offset_ms = (start as i64 * 1000) / SAMPLE_RATE as i64;
            for i in 0..state.full_n_segments() {
                let Some(seg) = state.get_segment(i) else {
                    continue;
                };
                let raw = seg.to_str_lossy()?;
                let Some(text) = strip_prompt_leak(raw.trim(), prompt) else {
                    continue;
                };
                if is_known_hallucination(&text) {
                    eprintln!("[stt-md] dropped hallucinated segment: {text}");
                    continue;
                }
                segments.push(TranscriptSegment {
                    start_ms: offset_ms + seg.start_timestamp() * 10,
                    end_ms: offset_ms + seg.end_timestamp() * 10,
                    text,
                    speaker: None,
                });
            }
        }

        drop_repetition_runs(&mut segments);
        Ok(segments)
    }
}

const SAMPLE_RATE: usize = 16_000;
const FRAME: usize = SAMPLE_RATE / 50; // 20 ms
const MAX_WINDOW: usize = 28 * SAMPLE_RATE;
const MIN_WINDOW: usize = 18 * SAMPLE_RATE;
/// Absolute floor below which a frame is silence regardless of the recording's
/// noise level (~-45 dBFS). Room noise after a meeting sits around -50 dBFS
/// and is exactly what whisper turns into "Gracias por ver el video".
const SILENCE_FLOOR_RMS: f32 = 0.0056;
/// Voiced frames a window needs to be worth decoding (1 s): a cough, a door
/// or a chair isn't speech.
const MIN_VOICED_FRAMES: usize = 50;

fn frame_rms(samples: &[f32]) -> Vec<f32> {
    samples
        .chunks(FRAME)
        .map(|f| (f.iter().map(|x| x * x).sum::<f32>() / f.len() as f32).sqrt())
        .collect()
}

/// Splits the buffer into windows no longer than `MAX_WINDOW`, cutting at the
/// quietest frame between `MIN_WINDOW` and `MAX_WINDOW` so words aren't split,
/// and drops windows with no frame above the speech threshold.
fn speech_windows(samples: &[f32]) -> Vec<(usize, usize)> {
    let rms = frame_rms(samples);
    if rms.is_empty() {
        return Vec::new();
    }
    let mut sorted = rms.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let noise_floor = sorted[sorted.len() / 10];
    let threshold = (noise_floor * 4.0).max(SILENCE_FLOOR_RMS);

    let max_frames = MAX_WINDOW / FRAME;
    let min_frames = MIN_WINDOW / FRAME;
    let mut windows = Vec::new();
    let mut start = 0;
    while start < rms.len() {
        let end = if rms.len() - start <= max_frames {
            rms.len()
        } else {
            let lo = start + min_frames;
            let hi = start + max_frames;
            (lo..hi)
                .min_by(|&a, &b| rms[a].total_cmp(&rms[b]))
                .map_or(hi, |i| i + 1)
        };
        let voiced = rms[start..end].iter().filter(|&&r| r > threshold).count();
        if voiced >= MIN_VOICED_FRAMES {
            windows.push((start * FRAME, (end * FRAME).min(samples.len())));
        }
        start = end;
    }
    windows
}

/// Phrases whisper produces from noise or silence, learned from subtitle
/// credits in its training data. Matched against the whole segment only, so
/// a real "gracias" inside a sentence survives.
fn is_known_hallucination(text: &str) -> bool {
    const PHRASES: &[&str] = &[
        "gracias por ver el video",
        "gracias por ver",
        "suscríbete al canal",
        "suscríbete",
        "subtítulos realizados por la comunidad de amara.org",
        "subtítulos por la comunidad de amara.org",
        "thanks for watching",
        "thank you for watching",
    ];
    let t = text
        .trim()
        .trim_matches(|c: char| c.is_ascii_punctuation() || c == '¡' || c == '¿')
        .to_lowercase();
    // Sound-effect captions: "*sad music*", "[Música]", "(risas)".
    let bracketed = [('*', '*'), ('[', ']'), ('(', ')')]
        .iter()
        .any(|&(o, c)| text.trim().starts_with(o) && text.trim().ends_with(c));
    bracketed || PHRASES.contains(&t.as_str())
}

/// whisper.cpp sometimes echoes the initial prompt back as if it had been
/// spoken ("Vocabulario común: …"). Strips a leading prompt phrase from the
/// segment and drops segments that are nothing but prompt text.
fn strip_prompt_leak(text: &str, prompt: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let prompt_lower = prompt.to_lowercase();
    if lower.len() >= 8 && prompt_lower.contains(lower.trim_end_matches(['.', ','])) {
        return None;
    }
    let mut rest = text;
    for phrase in prompt.split(['.', ':']).map(str::trim).filter(|p| p.len() >= 8) {
        if rest.to_lowercase().starts_with(&phrase.to_lowercase())
            && let Some(tail) = rest.get(phrase.len()..)
        {
            rest = tail.trim_start_matches([':', '.', ',', ' ']);
        }
    }
    let rest = rest.trim();
    (!rest.is_empty()).then(|| rest.to_string())
}

fn num_cpus_for_whisper() -> std::os::raw::c_int {
    // Leave one core for the UI / OS.
    let physical = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    (physical.saturating_sub(1).max(1)) as _
}

/// whisper.cpp can get stuck repeating the same phrase verbatim over
/// silence or noise. Removes every run of identical consecutive segments long
/// enough to be a hallucination loop, keeping what comes after it: with
/// windowed decoding a loop in one stretch says nothing about the rest of the
/// meeting.
///
/// The threshold is deliberately high: real conversation legitimately
/// repeats short interjections ("No. No. No. No.", "¿Puedo? ¿Puedo?
/// ¿Puedo? ¿Puedo?") up to ~10 times in a row, while observed whisper.cpp
/// hallucination loops run 15-140+ times once they start.
fn drop_repetition_runs(segments: &mut Vec<TranscriptSegment>) {
    const RUN_THRESHOLD: usize = 20;
    let normalized: Vec<String> = segments
        .iter()
        .map(|s| s.text.trim().to_lowercase())
        .collect();
    let mut keep = vec![true; segments.len()];
    let mut run_start = 0;
    for i in 1..=normalized.len() {
        if i < normalized.len() && normalized[i] == normalized[i - 1] {
            continue;
        }
        if i - run_start >= RUN_THRESHOLD {
            keep[run_start..i].fill(false);
        }
        run_start = i;
    }
    let mut keep = keep.into_iter();
    segments.retain(|_| keep.next().unwrap_or(true));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(text: &str) -> TranscriptSegment {
        TranscriptSegment {
            start_ms: 0,
            end_ms: 0,
            text: text.to_string(),
            speaker: None,
        }
    }

    fn texts(segments: &[TranscriptSegment]) -> Vec<&str> {
        segments.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn drop_repetition_runs_keeps_speech_after_loop() {
        let mut segments = vec![seg("hola"), seg("como estas")];
        segments.extend((0..20).map(|_| seg("Eu sei que ainda não terminam.")));
        segments.push(seg("seguimos"));
        drop_repetition_runs(&mut segments);
        assert_eq!(texts(&segments), vec!["hola", "como estas", "seguimos"]);
    }

    #[test]
    fn drop_repetition_runs_is_case_insensitive() {
        let mut segments = vec![seg("hola")];
        for i in 0..20 {
            segments.push(seg(if i % 2 == 0 {
                "Eu sei que ainda não terminam."
            } else {
                "EU SEI QUE AINDA NÃO TERMINAM."
            }));
        }
        drop_repetition_runs(&mut segments);
        assert_eq!(texts(&segments), vec!["hola"]);
    }

    #[test]
    fn drop_repetition_runs_ignores_short_runs() {
        // Real dialogue legitimately repeats short interjections many times
        // in a row ("No. No. No. No.", "¿Puedo? ¿Puedo? ¿Puedo? ¿Puedo?").
        // Observed max in real transcripts: 11 in a row.
        let mut segments = vec![seg("antes")];
        segments.extend((0..11).map(|_| seg("¿Puedo?")));
        segments.push(seg("despues"));
        drop_repetition_runs(&mut segments);
        assert_eq!(segments.len(), 13);
    }

    #[test]
    fn known_hallucinations_are_whole_segment_matches() {
        assert!(is_known_hallucination("Gracias por ver el video."));
        assert!(is_known_hallucination("¡Suscríbete al canal!"));
        assert!(is_known_hallucination("*sad music*"));
        assert!(is_known_hallucination("[Música]"));
        assert!(!is_known_hallucination("Gracias."));
        assert!(!is_known_hallucination("Gracias por ver el video que mandaste ayer."));
        assert!(!is_known_hallucination("Sí, (bueno) eso."));
    }

    #[test]
    fn strip_prompt_leak_removes_leading_prompt_phrase() {
        let prompt = "Reunión técnica en español. Vocabulario común: Ollama, vault.";
        assert_eq!(
            strip_prompt_leak("Vocabulario común: Te mando el archivo después.", prompt).as_deref(),
            Some("Te mando el archivo después.")
        );
        assert_eq!(strip_prompt_leak("Vocabulario común:", prompt), None);
        assert_eq!(strip_prompt_leak("Ollama, vault.", prompt), None);
        assert_eq!(strip_prompt_leak("Sí.", prompt).as_deref(), Some("Sí."));
        assert_eq!(
            strip_prompt_leak("Hablemos del vault", prompt).as_deref(),
            Some("Hablemos del vault")
        );
    }

    fn tone(secs: f32) -> Vec<f32> {
        (0..(secs * SAMPLE_RATE as f32) as usize)
            .map(|i| 0.2 * (i as f32 * 0.05).sin())
            .collect()
    }

    #[test]
    fn speech_windows_skip_silence_and_cap_length() {
        let mut audio = tone(40.0);
        audio.extend(vec![0.0; 60 * SAMPLE_RATE]);
        audio.extend(tone(10.0));
        let windows = speech_windows(&audio);
        assert!(windows.iter().all(|(s, e)| e - s <= MAX_WINDOW));
        let covered: usize = windows.iter().map(|(s, e)| e - s).sum();
        assert!(covered < 80 * SAMPLE_RATE, "silence was not skipped: {covered}");
        assert_eq!(windows.last().unwrap().1, audio.len());
        assert_eq!(windows[0].0, 0);
    }

    #[test]
    fn speech_windows_empty_for_silence() {
        assert!(speech_windows(&vec![0.0; 120 * SAMPLE_RATE]).is_empty());
        assert!(speech_windows(&[]).is_empty());
    }

    #[test]
    fn drop_repetition_runs_noop_when_all_distinct() {
        let mut segments = vec![seg("a"), seg("b"), seg("c")];
        drop_repetition_runs(&mut segments);
        assert_eq!(texts(&segments), vec!["a", "b", "c"]);
    }
}
