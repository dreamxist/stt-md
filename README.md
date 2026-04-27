# stt-md

Lightweight macOS menubar app that records meetings, transcribes locally with Whisper, summarizes with a local LLM (Ollama), and writes structured Markdown notes directly to your Obsidian vault — with tags inferred from your existing vocabulary so it doesn't pollute your notes with hallucinated terms.

```
                ┌─────────────────────────────────┐
  click ●  →    │ ● grabando 12:34                │  ← live timer in the menubar
                │ ─────────────────────────────── │
                │ Detener — 12:34                 │
                │ Salir                           │
                └─────────────────────────────────┘
                              ↓ stop
        whisper.cpp (Metal) ─→ Ollama ─→ vault/2-calendar/YYYY/MM/meetings/<slug>.md
                                       └→ append link to today's daily ## 🤖 Agent Log
```

100% local. No cloud. No telemetry. ~3 MB binary.

## Why

[stt-tomi](https://github.com/Zackriya-Solutions/meeting-minutes) (the project this learned from) is great but bundles Tauri + Next.js + FastAPI + SQLite for a 1.6 GB install. If all you need is mic-only meeting notes that land in your Obsidian vault, that's a lot of moving parts. **stt-md** strips it down to a single Rust binary with a native AppKit tray icon, ~2000 lines of code.

| | stt-tomi | stt-md |
|---|---|---|
| Bundle | ~1.6 GB | ~3 MB |
| Cold start | 1–2 s | <500 ms |
| Backend | FastAPI + SQLite | none (flat files) |
| UI | Tauri + Next.js | tray-icon + tao |
| System audio | yes (BlackHole) | no, mic only |
| Streaming transcription | yes (VAD) | no, batch at stop |
| Output | SQLite + JSON | Markdown directly to vault |
| Tag intelligence | none | reuses vocab from your vault |

## Requirements

- macOS 11+ on Apple Silicon (Metal GPU)
- [Ollama](https://ollama.ai) running locally with a model pulled:
  ```bash
  brew install ollama
  ollama pull qwen2.5:7b
  ```
- A Whisper.cpp GGML model in `~/Library/Application Support/stt-md/models/`. The app expects `ggml-large-v3-turbo.bin` by default (~1.6 GB):
  ```bash
  mkdir -p "$HOME/Library/Application Support/stt-md/models"
  curl -L -o "$HOME/Library/Application Support/stt-md/models/ggml-large-v3-turbo.bin" \
    https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin
  ```
- Rust 1.77+ (only to build)

## Build & install

```bash
git clone https://github.com/dreamxist/stt-md
cd stt-md
make build           # cargo build --release + bundle dist/stt-md.app
open dist/stt-md.app # first launch — macOS will ask for mic permission
```

To launch automatically at login (macOS Login Items):

```bash
osascript -e 'tell application "System Events" to make login item at end \
  with properties {path:"/full/path/to/stt-md.app", hidden:true}'
```

## Configuration

On first launch, `~/Library/Application Support/stt-md/config.toml` is created:

```toml
vault_root = "/Users/<you>/path/to/your-vault"
ollama_model = "qwen2.5:7b"
ollama_url = "http://localhost:11434"
whisper_language = "es"
whisper_model_filename = "ggml-large-v3-turbo.bin"
```

Edit it to point at your Obsidian vault and (optionally) swap the LLM or Whisper model.

## How it works

1. **Click `STT` in menubar → "Empezar reunión"**. The icon shows a live timer (`STT 12:34`). Audio captured via `cpal` at the device's native sample rate, written to `~/Library/Application Support/stt-md/recordings/`.
2. **Click "Detener"**. Icon switches to `STT …`. In a worker thread:
   - Audio is loaded, downmixed to mono, resampled to 16 kHz.
   - **Whisper** transcribes in batch with Metal acceleration (`whisper-rs` + `large-v3-turbo`).
   - The vault is scanned (`walkdir`) for existing tags (frontmatter `tags:` and inline `#tag`) and wikilink targets (`.md` filenames).
   - **Ollama** is asked for a structured JSON summary, with the existing tag vocabulary inserted into the prompt as a closed list.
   - Hallucinated tags / wikilinks are filtered out post-hoc against the actual vault contents (LLMs invent things; the vault is the source of truth).
3. The Markdown file lands in `<vault>/2-calendar/YYYY/MM/meetings/YYYY-MM-DD-HHMM-<slug>.md` with frontmatter, summary, decisions, action items, people, and a collapsible full transcript.
4. A line is appended to today's daily note under `## 🤖 Agent Log`.
5. macOS notification fires when done.

## Output format

```yaml
---
date: 2026-04-26
day: domingo
time: 14:30
title: HeyMark standup
duration_min: 32
tags: [meeting, ai-draft, heymark]
people: [juan, maria, pancho]
project: "[[heymark]]"
audio: 2026-04-26-1430-heymark-standup.wav
type: meeting
source: stt-md
---

# HeyMark standup

## Resumen
- Bullet 1
- Bullet 2

## Decisiones
- ...

## Action items
- [ ] @juan — ... *(deadline: 2026-04-30)*

## Personas
- [[juan]]

## Transcripción
<details>
<summary>Ver transcripción completa</summary>

[00:00] ...
</details>
```

## Project layout

```
src/
├── main.rs              # tao event loop + tray + state machine
├── lib.rs               # module re-exports
├── app_state.rs         # enum AppState { Idle, Recording, Processing }
├── config.rs            # TOML config in Application Support
├── paths.rs             # filesystem helpers
├── sounds.rs            # afplay wrappers (Tink / Pop)
├── notifications.rs     # notify-rust for macOS notifications
├── audio_utils.rs       # WAV load + mono + linear resample
├── recording/
│   ├── mic.rs           # cpal stream
│   ├── wav_writer.rs    # hound writer on its own thread
│   └── mod.rs
├── transcription/
│   └── whisper.rs       # whisper-rs wrapper with initial_prompt
├── llm/
│   ├── ollama.rs        # /api/generate with format=json
│   ├── prompts.rs       # structured-summary prompt with vault vocabulary
│   └── mod.rs           # MeetingSummary + enforce_vocab()
├── vault/
│   ├── scanner.rs       # walkdir → tags + wikilink targets
│   ├── meeting_writer.rs# generate the meeting .md
│   └── daily_appender.rs# append link to ## 🤖 Agent Log
└── bin/
    ├── transcribe-wav.rs # CLI: WAV → bare-transcript .md (no LLM)
    ├── test-summary.rs   # CLI: prompt + Ollama dry run
    └── test-e2e.rs       # CLI: full pipeline against any vault
```

## Helpers

```bash
# Just transcribe a WAV (no Ollama, no vault):
./target/release/transcribe-wav path/to/audio.wav title

# Dry-run the prompt + Ollama against your vault (set STT_MD_VAULT):
STT_MD_VAULT="$HOME/path/to/vault" ./target/release/test-summary

# Full pipeline against any vault:
./target/release/test-e2e path/to/audio.wav path/to/vault
```

## Known limitations

- **Mic only.** No system-audio capture (Zoom/Meet). Use [stt-tomi](https://github.com/Zackriya-Solutions/meeting-minutes) with BlackHole if you need that.
- **Batch at stop**, not streaming. A 30-min meeting takes ~30–60 s of post-processing.
- **Linear resampling** with no anti-aliasing low-pass. Fine for speech (80–4000 Hz). Swap for `rubato` if you need fidelity.
- **No code signing.** macOS will ask "Open Anyway" once per build. For wider distribution you'd need an Apple Developer ID ($99/year).
- **No fancy date math.** Relative deadlines like "next Thursday" depend on the LLM doing the right calendar arithmetic; failures are filtered to `null`.

## Roadmap (maybe)

- [ ] VAD streaming during recording (port from stt-tomi's Silero pipeline)
- [ ] Configurable output schema (currently Spanish-Obsidian opinionated)
- [ ] Re-process command on existing `.md` (re-run the summarizer with a different model)
- [ ] System audio via Core Audio TAP (no BlackHole required on macOS 14.4+)
- [ ] Code signing + DMG distribution

## License

MIT — see [LICENSE](./LICENSE).

## Acknowledgements

- [whisper.cpp](https://github.com/ggerganov/whisper.cpp) — local STT
- [Ollama](https://ollama.ai) — local LLM runtime
- [stt-tomi / Meetily](https://github.com/Zackriya-Solutions/meeting-minutes) — the heavyweight cousin this learned from
- [tao](https://github.com/tauri-apps/tao) + [tray-icon](https://github.com/tauri-apps/tray-icon) — native menubar without a webview
