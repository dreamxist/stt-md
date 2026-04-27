use stt_md::{
    app_state, audio_utils, config, llm, notifications, recording, sounds, transcription, vault,
};

use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use crossbeam_channel::{unbounded, Receiver, Sender};
use parking_lot::Mutex;
use tao::event_loop::{ControlFlow, EventLoop};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

use app_state::AppState;
use config::Config;
use recording::RecordingSession;
use transcription::whisper::WhisperEngine;

enum ProcessingMsg {
    Done(PathBuf),
    Failed(String),
}

fn main() -> anyhow::Result<()> {
    let cfg = Config::load_or_init()?;
    println!("[stt-md] config loaded from {}", Config::config_path().display());
    println!("[stt-md] vault: {}", cfg.vault_root.display());
    let cfg = Arc::new(cfg);

    let event_loop = EventLoop::new();

    let start_item = MenuItem::new("Empezar reunión", true, None);
    let stop_item = MenuItem::new("Detener", false, None);
    let separator = PredefinedMenuItem::separator();
    let quit_item = MenuItem::new("Salir", true, None);

    let mut tray = Some(build_tray(&start_item, &stop_item, &separator, &quit_item)?);

    let menu_channel = MenuEvent::receiver();
    let start_id = start_item.id().clone();
    let stop_id = stop_item.id().clone();
    let quit_id = quit_item.id().clone();

    let state = Arc::new(Mutex::new(AppState::Idle));
    let session: Arc<Mutex<Option<(RecordingSession, DateTime<Local>)>>> =
        Arc::new(Mutex::new(None));
    let whisper: Arc<Mutex<Option<Arc<WhisperEngine>>>> = Arc::new(Mutex::new(None));

    let (proc_tx, proc_rx): (Sender<ProcessingMsg>, Receiver<ProcessingMsg>) = unbounded();

    // Refresh tooltip + stop-item label every ~1s during recording/processing.
    // We avoid `set_title` entirely: tray-icon 0.19 + AppKit can't reliably
    // clear a sticky title afterwards, so timer goes in the tooltip and the
    // disabled stop-item's label instead.
    let mut last_tick = Instant::now() - Duration::from_secs(2);

    event_loop.run(move |_event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(250));

        let st = *state.lock();
        match st {
            AppState::Recording { started_at } => {
                if last_tick.elapsed() >= Duration::from_millis(900) {
                    let elapsed = started_at.elapsed().as_secs();
                    let mins = elapsed / 60;
                    let secs = elapsed % 60;
                    let timer = format!(" {:02}:{:02}", mins, secs);
                    if let Some(t) = tray.as_ref() {
                        let _ = t.set_title(Some(timer.clone()));
                        let _ = t.set_tooltip(Some(format!("● grabando {:02}:{:02}", mins, secs)));
                    }
                    stop_item.set_text(format!("Detener — {:02}:{:02}", mins, secs));
                    last_tick = Instant::now();
                }
            }
            AppState::Processing => {
                if last_tick.elapsed() >= Duration::from_millis(900) {
                    if let Some(t) = tray.as_ref() {
                        let _ = t.set_title(Some(" …".to_string()));
                        let _ = t.set_tooltip(Some("Procesando transcripción…".to_string()));
                    }
                    last_tick = Instant::now();
                }
            }
            AppState::Idle => {}
        }

        while let Ok(event) = menu_channel.try_recv() {
            if event.id == quit_id {
                if let Some((s, _)) = session.lock().take() {
                    let _ = s.stop();
                }
                tray.take();
                *control_flow = ControlFlow::Exit;
            } else if event.id == start_id {
                let mut st = state.lock();
                if matches!(*st, AppState::Idle) {
                    match RecordingSession::start() {
                        Ok(s) => {
                            sounds::play_start();
                            let started_at = s.started_at;
                            let now = Local::now();
                            *session.lock() = Some((s, now));
                            *st = AppState::Recording { started_at };
                            start_item.set_enabled(false);
                            stop_item.set_enabled(true);
                            stop_item.set_text("Detener — 00:00");
                            if let Some(t) = tray.as_ref() {
                                let _ = t.set_title(Some(" 00:00".to_string()));
                                let _ = t.set_tooltip(Some("● grabando 00:00".to_string()));
                            }
                            println!("[stt-md] recording started");
                        }
                        Err(e) => eprintln!("[stt-md] failed to start recording: {e:?}"),
                    }
                }
            } else if event.id == stop_id {
                let mut st = state.lock();
                if st.is_recording() {
                    if let Some((s, started_at_local)) = session.lock().take() {
                        sounds::play_stop();
                        let started_at_inst = s.started_at;
                        let duration_min =
                            (started_at_inst.elapsed().as_secs() / 60).max(1) as i64;
                        match s.stop() {
                            Ok(wav_path) => {
                                println!("[stt-md] saved {}", wav_path.display());
                                *st = AppState::Processing;
                                start_item.set_enabled(false);
                                stop_item.set_enabled(false);
                                stop_item.set_text("Procesando…");
                                if let Some(t) = tray.as_ref() {
                                    let _ = t.set_tooltip(Some(
                                        "Procesando transcripción…".to_string(),
                                    ));
                                }
                                last_tick = Instant::now() - Duration::from_secs(2);

                                let whisper_clone = whisper.clone();
                                let proc_tx_clone = proc_tx.clone();
                                let cfg_clone = cfg.clone();
                                thread::spawn(move || {
                                    let msg = match process_recording(
                                        &wav_path,
                                        started_at_local,
                                        duration_min,
                                        &whisper_clone,
                                        &cfg_clone,
                                    ) {
                                        Ok(p) => ProcessingMsg::Done(p),
                                        Err(e) => {
                                            eprintln!("[stt-md] processing error: {e:?}");
                                            ProcessingMsg::Failed(e.to_string())
                                        }
                                    };
                                    let _ = proc_tx_clone.send(msg);
                                });
                            }
                            Err(e) => {
                                eprintln!("[stt-md] stop error: {e:?}");
                                *st = AppState::Idle;
                                start_item.set_enabled(true);
                                stop_item.set_enabled(false);
                            }
                        }
                    }
                }
            }
        }

        while let Ok(msg) = proc_rx.try_recv() {
            match msg {
                ProcessingMsg::Done(path) => {
                    println!("[stt-md] wrote {}", path.display());
                    notifications::meeting_saved(&path);
                }
                ProcessingMsg::Failed(err) => {
                    eprintln!("[stt-md] processing failed: {err}");
                    notifications::meeting_failed(&err);
                }
            }
            *state.lock() = AppState::Idle;
            start_item.set_enabled(true);
            stop_item.set_enabled(false);
            stop_item.set_text("Detener");
            // Drop + rebuild the TrayIcon to clear the sticky " …" title.
            // tray-icon 0.19 macOS has no working API to clear once a title
            // is set; only fully recreating the NSStatusItem works.
            tray.take();
            match build_tray(&start_item, &stop_item, &separator, &quit_item) {
                Ok(t) => {
                    let _ = t.set_tooltip(Some("stt-md".to_string()));
                    tray = Some(t);
                }
                Err(e) => eprintln!("[stt-md] failed to rebuild tray: {e:?}"),
            }
            last_tick = Instant::now() - Duration::from_secs(2);
        }
    });
}

fn build_tray(
    start: &MenuItem,
    stop: &MenuItem,
    sep: &PredefinedMenuItem,
    quit: &MenuItem,
) -> anyhow::Result<tray_icon::TrayIcon> {
    let menu = Menu::new();
    menu.append_items(&[start, stop, sep, quit])?;
    Ok(TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(build_idle_icon())
        .with_tooltip("stt-md")
        .with_icon_as_template(true)
        .build()?)
}

fn process_recording(
    wav_path: &std::path::Path,
    started_at_local: DateTime<Local>,
    duration_min: i64,
    whisper_slot: &Arc<Mutex<Option<Arc<WhisperEngine>>>>,
    cfg: &Config,
) -> anyhow::Result<PathBuf> {
    let t0 = Instant::now();
    let (mono, sr) = audio_utils::load_wav_mono_f32(wav_path)?;
    let resampled = audio_utils::resample_to_16k(&mono, sr);
    println!(
        "[stt-md] loaded {:.1}s of audio in {}ms",
        resampled.len() as f32 / 16_000.0,
        t0.elapsed().as_millis()
    );

    let engine = {
        let mut slot = whisper_slot.lock();
        if slot.is_none() {
            let model_path = cfg.whisper_model_path();
            println!("[stt-md] loading whisper model from {}", model_path.display());
            let t0 = Instant::now();
            let engine = WhisperEngine::load(&model_path)?;
            println!("[stt-md] model loaded in {}ms", t0.elapsed().as_millis());
            *slot = Some(Arc::new(engine));
        }
        slot.as_ref().unwrap().clone()
    };

    let t0 = Instant::now();
    let segments = engine.transcribe(&resampled)?;
    println!(
        "[stt-md] transcribed {} segments in {}ms",
        segments.len(),
        t0.elapsed().as_millis()
    );

    let vault_root = cfg.vault_root.as_path();

    let t0 = Instant::now();
    let vocab = vault::scanner::scan_vault(vault_root)?;
    println!(
        "[stt-md] scanned vault in {}ms ({} tags / {} wikilinks)",
        t0.elapsed().as_millis(),
        vocab.frontmatter_tags.len() + vocab.inline_tags.len(),
        vocab.wikilink_targets.len()
    );

    let transcript_text: String = segments
        .iter()
        .map(|s| {
            let mins = s.start_ms / 60_000;
            let secs = (s.start_ms % 60_000) / 1000;
            format!("[{:02}:{:02}] {}\n", mins, secs, s.text)
        })
        .collect();

    let prompt = llm::prompts::build_summary_prompt(&transcript_text, &vocab);

    println!("[stt-md] calling Ollama ({}) — this can take 20-60s…", cfg.ollama_model);
    let t0 = Instant::now();
    let raw = llm::ollama::generate_json(&prompt, &cfg.ollama_model, &cfg.ollama_url)?;
    println!("[stt-md] ollama replied in {}ms", t0.elapsed().as_millis());

    let mut summary: llm::MeetingSummary = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("ollama returned invalid JSON: {e}\n--- raw ---\n{raw}"))?;
    summary.enforce_vocab(&vocab);

    let written = vault::meeting_writer::write_meeting(
        vault_root,
        started_at_local,
        &summary,
        &segments,
        duration_min,
        wav_path,
    )?;
    println!("[stt-md] wrote meeting to {}", written.absolute_path.display());

    let daily_path = vault::daily_appender::append_meeting_link(
        vault_root,
        started_at_local,
        &summary.title,
        &written.vault_relative,
        duration_min,
    )?;
    println!("[stt-md] updated daily {}", daily_path.display());

    Ok(written.absolute_path)
}

fn build_idle_icon() -> Icon {
    // 20x20. "STT" en pixel art (5x7 por letra, separador 1px).
    let pattern: [&str; 20] = [
        "....................",
        "....................",
        "....................",
        "....................",
        "....................",
        "....................",
        "..####.#####.#####..",
        ".#........#.....#...",
        ".#........#.....#...",
        "..###.....#.....#...",
        ".....#....#.....#...",
        ".....#....#.....#...",
        ".####.....#.....#...",
        "....................",
        "....................",
        "....................",
        "....................",
        "....................",
        "....................",
        "....................",
    ];
    rgba_from_pattern(&pattern)
}

fn rgba_from_pattern(pattern: &[&str]) -> Icon {
    let size = pattern.len() as u32;
    debug_assert!(pattern.iter().all(|r| r.chars().count() == size as usize));
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for (y, row) in pattern.iter().enumerate() {
        for (x, ch) in row.chars().enumerate() {
            if ch == '#' {
                let idx = ((y as u32 * size + x as u32) * 4) as usize;
                rgba[idx + 3] = 255;
            }
        }
    }
    Icon::from_rgba(rgba, size, size).expect("valid RGBA buffer")
}
