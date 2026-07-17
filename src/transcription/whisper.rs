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
    pub fn transcribe(
        &self,
        samples_16k_mono: &[f32],
        language: &str,
        initial_prompt: Option<&str>,
    ) -> Result<Vec<TranscriptSegment>> {
        let mut state = self.ctx.create_state()?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(language));
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_n_threads(num_cpus_for_whisper());
        params.set_initial_prompt(initial_prompt.unwrap_or(DEFAULT_INITIAL_PROMPT));

        state.full(params, samples_16k_mono)?;

        let n = state.full_n_segments();
        let mut segments = Vec::with_capacity(n as usize);
        for i in 0..n {
            let Some(seg) = state.get_segment(i) else {
                continue;
            };
            let text = seg.to_str_lossy()?.trim().to_string();
            if text.is_empty() {
                continue;
            }
            segments.push(TranscriptSegment {
                start_ms: seg.start_timestamp() * 10,
                end_ms: seg.end_timestamp() * 10,
                text,
                speaker: None,
            });
        }
        if let Some(cutoff) = repetition_cutoff(&segments) {
            segments.truncate(cutoff);
        }
        Ok(segments)
    }
}

fn num_cpus_for_whisper() -> std::os::raw::c_int {
    // Leave one core for the UI / OS.
    let physical = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    (physical.saturating_sub(1).max(1)) as _
}

/// whisper.cpp can get stuck repeating the same phrase verbatim once real
/// speech ends (e.g. silence tailing a long recording after everyone hangs
/// up). Returns the index where a run of identical consecutive segments
/// starts, so the caller can drop the hallucinated tail.
///
/// The threshold is deliberately high: real conversation legitimately
/// repeats short interjections ("No. No. No. No.", "¿Puedo? ¿Puedo?
/// ¿Puedo? ¿Puedo?") up to ~10 times in a row, while observed whisper.cpp
/// hallucination loops run 15-140+ times once they start.
fn repetition_cutoff(segments: &[TranscriptSegment]) -> Option<usize> {
    const RUN_THRESHOLD: usize = 20;
    let normalized: Vec<String> = segments
        .iter()
        .map(|s| s.text.trim().to_lowercase())
        .collect();
    let mut run_start = 0;
    for i in 1..normalized.len() {
        if normalized[i] == normalized[i - 1] {
            if i - run_start + 1 >= RUN_THRESHOLD {
                return Some(run_start);
            }
        } else {
            run_start = i;
        }
    }
    None
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

    #[test]
    fn repetition_cutoff_finds_start_of_long_run() {
        let mut segments = vec![seg("hola"), seg("como estas")];
        segments.extend((0..20).map(|_| seg("Eu sei que ainda não terminam.")));
        assert_eq!(repetition_cutoff(&segments), Some(2));
    }

    #[test]
    fn repetition_cutoff_is_case_insensitive() {
        let mut segments = vec![seg("hola")];
        for i in 0..20 {
            segments.push(seg(if i % 2 == 0 {
                "Eu sei que ainda não terminam."
            } else {
                "EU SEI QUE AINDA NÃO TERMINAM."
            }));
        }
        assert_eq!(repetition_cutoff(&segments), Some(1));
    }

    #[test]
    fn repetition_cutoff_ignores_short_runs() {
        // Real dialogue legitimately repeats short interjections many times
        // in a row ("No. No. No. No.", "¿Puedo? ¿Puedo? ¿Puedo? ¿Puedo?").
        // Observed max in real transcripts: 11 in a row.
        let mut segments = vec![seg("antes")];
        segments.extend((0..11).map(|_| seg("¿Puedo?")));
        segments.push(seg("despues"));
        assert_eq!(repetition_cutoff(&segments), None);
    }

    #[test]
    fn repetition_cutoff_none_when_all_distinct() {
        let segments = vec![seg("a"), seg("b"), seg("c")];
        assert_eq!(repetition_cutoff(&segments), None);
    }
}
