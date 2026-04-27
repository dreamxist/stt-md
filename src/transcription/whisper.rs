use anyhow::{Context, Result};
use std::path::Path;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::TranscriptSegment;

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

    /// Transcribe 16kHz mono f32 samples in Spanish.
    pub fn transcribe(&self, samples_16k_mono: &[f32]) -> Result<Vec<TranscriptSegment>> {
        let mut state = self.ctx.create_state()?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("es"));
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_n_threads(num_cpus_for_whisper());
        params.set_initial_prompt(
            "Reunión técnica de software en español chileno. \
             Vocabulario común: whisper, Ollama, Obsidian, vault, prompt, LLM, \
             stt-md, HeyMark, IMC Labs, Tauri, Rust, TypeScript, Next.js, React, \
             Supabase, AWS, Anthropic, Claude, MCP, agente, embedding, \
             frontend, backend, deploy, sprint, standup, PR, merge, commit.",
        );

        state.full(params, samples_16k_mono)?;

        let n = state.full_n_segments();
        let mut segments = Vec::with_capacity(n as usize);
        for i in 0..n {
            let Some(seg) = state.get_segment(i) else {
                continue;
            };
            let text = seg.to_str()?.trim().to_string();
            if text.is_empty() {
                continue;
            }
            segments.push(TranscriptSegment {
                start_ms: seg.start_timestamp() * 10,
                end_ms: seg.end_timestamp() * 10,
                text,
            });
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
