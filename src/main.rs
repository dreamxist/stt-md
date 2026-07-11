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
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

use app_state::AppState;
use config::{Config, OutputMode};
use recording::RecordingSession;
use transcription::whisper::WhisperEngine;

enum ProcessingMsg {
    Done(PathBuf),
    Failed(String),
}

type SessionSlot = Arc<Mutex<Option<(RecordingSession, DateTime<Local>)>>>;

fn main() {
    if let Err(e) = run() {
        let msg = format!("{e:#}");
        eprintln!("[stt-md] fatal: {msg}");
        // Launched as .app there is no terminal: surface the error visually
        // or the app just silently never appears in the menubar.
        show_fatal_alert(&msg);
        std::process::exit(1);
    }
}

fn show_fatal_alert(msg: &str) {
    let script = format!(
        "display alert \"stt-md no pudo iniciar\" message \"{}\" as critical",
        msg.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let _ = std::process::Command::new("osascript")
        .args(["-e", &script])
        .status();
}

fn validate_startup(cfg: &Config) -> anyhow::Result<()> {
    let model_path = cfg.whisper_model_path();
    if !model_path.exists() {
        anyhow::bail!(
            "No se encontró el modelo Whisper en {}.\n\nDescárgalo con:\ncurl -L -o \"{}\" https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}",
            model_path.display(),
            model_path.display(),
            cfg.whisper_model_filename
        );
    }
    match cfg.output_mode {
        OutputMode::Obsidian => {
            if !cfg.vault_root.is_dir() {
                anyhow::bail!(
                    "vault_root no existe: {}.\n\nCorrige la ruta en {} (o usa output_mode = \"simple\").",
                    cfg.vault_root.display(),
                    Config::config_path().display()
                );
            }
        }
        OutputMode::Simple => {
            std::fs::create_dir_all(&cfg.output_dir).map_err(|e| {
                anyhow::anyhow!(
                    "no se pudo crear output_dir {}: {e}",
                    cfg.output_dir.display()
                )
            })?;
        }
    }
    Ok(())
}

fn run() -> anyhow::Result<()> {
    let cfg = Config::load_or_init()?;
    println!("[stt-md] config loaded from {}", Config::config_path().display());
    match cfg.output_mode {
        OutputMode::Obsidian => println!("[stt-md] mode: obsidian → {}", cfg.vault_root.display()),
        OutputMode::Simple => println!("[stt-md] mode: simple   → {}", cfg.output_dir.display()),
    }
    validate_startup(&cfg)?;
    let cfg = Arc::new(cfg);

    let mut event_loop = EventLoop::new();
    // Accessory = sin ícono en el Dock ni en Cmd-Tab; solo vive en la menubar.
    // tao fuerza Regular por defecto, pisando el LSUIElement del Info.plist.
    event_loop.set_activation_policy(ActivationPolicy::Accessory);

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
    let session: SessionSlot = Arc::new(Mutex::new(None));

    let (proc_tx, proc_rx): (Sender<ProcessingMsg>, Receiver<ProcessingMsg>) = unbounded();

    // Refresh title + tooltip + stop-item label every ~1s during
    // recording/processing. tray-icon 0.19 on macOS can't reliably clear a
    // sticky title once set, so when processing ends the tray is rebuilt
    // (see the proc_rx handler below).
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
                        t.set_title(Some(timer));
                        let _ = t.set_tooltip(Some(format!("● grabando {:02}:{:02}", mins, secs)));
                    }
                    stop_item.set_text(format!("Detener — {:02}:{:02}", mins, secs));
                    last_tick = Instant::now();
                }
            }
            AppState::Processing => {
                if last_tick.elapsed() >= Duration::from_millis(900) {
                    if let Some(t) = tray.as_ref() {
                        t.set_title(Some(" …"));
                        let _ = t.set_tooltip(Some("Procesando transcripción…"));
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
                    match RecordingSession::start_with_fallback() {
                        Ok((s, used_system)) => {
                            sounds::play_start();
                            let started_at = s.started_at;
                            let now = Local::now();
                            *session.lock() = Some((s, now));
                            *st = AppState::Recording { started_at };
                            start_item.set_enabled(false);
                            stop_item.set_enabled(true);
                            stop_item.set_text("Detener — 00:00");
                            let label = if used_system {
                                "● grabando (mic + sistema)"
                            } else {
                                "● grabando (solo mic)"
                            };
                            if let Some(t) = tray.as_ref() {
                                t.set_title(Some(" 00:00"));
                                let _ = t.set_tooltip(Some(format!("{label} 00:00")));
                            }
                            println!(
                                "[stt-md] recording started (system audio: {})",
                                if used_system { "yes" } else { "no — fallback to mic-only" }
                            );
                        }
                        Err(e) => {
                            eprintln!("[stt-md] failed to start recording: {e:?}");
                            notifications::recording_failed(&format!("{e:#}"));
                        }
                    }
                }
            } else if event.id == stop_id {
                let mut st = state.lock();
                if st.is_recording()
                    && let Some((s, started_at_local)) = session.lock().take()
                {
                    sounds::play_stop();
                    // Wall clock, not Instant: Instant stops while the Mac
                    // sleeps, undercounting long meetings.
                    let duration_min = (Local::now() - started_at_local)
                        .num_minutes()
                        .max(1);
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

                            let proc_tx_clone = proc_tx.clone();
                            let cfg_clone = cfg.clone();
                            thread::spawn(move || {
                                let msg = match process_recording(
                                    &wav_path,
                                    started_at_local,
                                    duration_min,
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

    if resampled.len() < 16_000 {
        anyhow::bail!(
            "la grabación dura menos de 1 segundo; no hay nada que transcribir (audio: {})",
            wav_path.display()
        );
    }

    // Load per recording instead of caching: keeping large-v3-turbo resident
    // costs ~1.5 GB of RAM forever; reloading adds only a few seconds to a
    // 30-60 s pipeline.
    let model_path = cfg.whisper_model_path();
    println!("[stt-md] loading whisper model from {}", model_path.display());
    let t0 = Instant::now();
    let engine = WhisperEngine::load(&model_path)?;
    println!("[stt-md] model loaded in {}ms", t0.elapsed().as_millis());

    let t0 = Instant::now();
    let segments = engine.transcribe(
        &resampled,
        &cfg.whisper_language,
        cfg.whisper_initial_prompt.as_deref(),
    )?;
    println!(
        "[stt-md] transcribed {} segments in {}ms",
        segments.len(),
        t0.elapsed().as_millis()
    );

    let transcript_text: String = segments
        .iter()
        .map(|s| {
            let mins = s.start_ms / 60_000;
            let secs = (s.start_ms % 60_000) / 1000;
            format!("[{:02}:{:02}] {}\n", mins, secs, s.text)
        })
        .collect();

    match cfg.output_mode {
        OutputMode::Obsidian => process_obsidian_mode(
            cfg,
            wav_path,
            started_at_local,
            duration_min,
            &segments,
            &transcript_text,
        ),
        OutputMode::Simple => process_simple_mode(
            cfg,
            wav_path,
            started_at_local,
            duration_min,
            &segments,
            &transcript_text,
        ),
    }
}

fn process_obsidian_mode(
    cfg: &Config,
    wav_path: &std::path::Path,
    started_at_local: DateTime<Local>,
    duration_min: i64,
    segments: &[transcription::TranscriptSegment],
    transcript_text: &str,
) -> anyhow::Result<PathBuf> {
    let vault_root = cfg.vault_root.as_path();

    let t0 = Instant::now();
    let vocab = vault::scanner::scan_vault(vault_root)?;
    let areas = cfg
        .areas_dir
        .as_deref()
        .map(|d| vault::scanner::scan_areas(vault_root, d))
        .unwrap_or_default();
    println!(
        "[stt-md] scanned vault in {}ms ({} tags / {} wikilinks / {} areas)",
        t0.elapsed().as_millis(),
        vocab.frontmatter_tags.len() + vocab.inline_tags.len(),
        vocab.wikilink_targets.len(),
        areas.len()
    );

    let prompt = llm::prompts::build_summary_prompt(transcript_text, &vocab, &areas);

    println!(
        "[stt-md] calling Ollama ({}) — this can take 20-60s…",
        cfg.ollama_model
    );
    let t0 = Instant::now();
    let raw = llm::ollama::generate_json(&prompt, &cfg.ollama_model, &cfg.ollama_url)?;
    println!("[stt-md] ollama replied in {}ms", t0.elapsed().as_millis());

    let mut summary: llm::MeetingSummary = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("ollama returned invalid JSON: {e}\n--- raw ---\n{raw}"))?;
    summary.enforce_vocab(&vocab);
    summary.enforce_area(&areas);

    let meetings_dir_rel = vault::expand_pattern(&cfg.meetings_dir, &started_at_local);
    let daily_note_rel = vault::expand_pattern(&cfg.daily_note, &started_at_local);
    let daily_template = cfg
        .daily_template
        .as_deref()
        .and_then(|rel| std::fs::read_to_string(vault_root.join(rel)).ok());

    let written = vault::meeting_writer::write_meeting(
        vault_root,
        &meetings_dir_rel,
        started_at_local,
        &summary,
        segments,
        duration_min,
        wav_path,
    )?;
    println!("[stt-md] wrote meeting to {}", written.absolute_path.display());

    let daily_path = vault::daily_appender::append_meeting_link(
        vault_root,
        &daily_note_rel,
        daily_template.as_deref(),
        started_at_local,
        &summary.title,
        &written.stem,
        duration_min,
    )?;
    println!("[stt-md] updated daily {}", daily_path.display());

    Ok(written.absolute_path)
}

fn process_simple_mode(
    cfg: &Config,
    wav_path: &std::path::Path,
    started_at_local: DateTime<Local>,
    duration_min: i64,
    segments: &[transcription::TranscriptSegment],
    transcript_text: &str,
) -> anyhow::Result<PathBuf> {
    let prompt = llm::prompts::build_simple_summary_prompt(transcript_text);

    println!(
        "[stt-md] calling Ollama ({}) — simple mode…",
        cfg.ollama_model
    );
    let t0 = Instant::now();
    let raw = llm::ollama::generate_json(&prompt, &cfg.ollama_model, &cfg.ollama_url)?;
    println!("[stt-md] ollama replied in {}ms", t0.elapsed().as_millis());

    let mut summary: llm::MeetingSummary = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("ollama returned invalid JSON: {e}\n--- raw ---\n{raw}"))?;
    summary.normalize_simple(4);

    let path = vault::simple_writer::write_simple(
        cfg.output_dir.as_path(),
        started_at_local,
        &summary,
        segments,
        duration_min,
        wav_path,
    )?;
    println!("[stt-md] wrote {}", path.display());

    Ok(path)
}

const APP_ICON_PNG: &[u8] = include_bytes!("../assets/icon-256.png");
const TRAY_ICON_SIZE: u32 = 20;

fn build_idle_icon() -> Icon {
    match glyph_icon_from_png(APP_ICON_PNG, TRAY_ICON_SIZE) {
        Ok(icon) => icon,
        Err(e) => {
            eprintln!("[stt-md] failed to build tray icon from app icon: {e:?}");
            fallback_idle_icon()
        }
    }
}

/// Extracts the light glyph (waveform + text lines) from the app icon and
/// turns it into a monochrome template image: luminance → alpha, so the dark
/// tile background disappears and macOS tints it for light/dark menubars.
fn glyph_icon_from_png(png_bytes: &[u8], size: u32) -> anyhow::Result<Icon> {
    let decoder = png::Decoder::new(png_bytes);
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf)?;
    let (w, h) = (info.width as usize, info.height as usize);
    let bpp = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        other => anyhow::bail!("unsupported app-icon color type: {other:?}"),
    };
    anyhow::ensure!(info.bit_depth == png::BitDepth::Eight, "expected 8-bit png");

    // Per-source-pixel glyph alpha, then area-average downscale for smooth edges.
    let glyph_alpha = |x: usize, y: usize| -> f32 {
        let p = (y * w + x) * bpp;
        let (r, g, b) = (buf[p] as f32, buf[p + 1] as f32, buf[p + 2] as f32);
        let a = if bpp == 4 { buf[p + 3] as f32 / 255.0 } else { 1.0 };
        let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        // Tile background is dark navy (lum ≈ 20-40); glyph is near-white.
        (((lum - 60.0) / 80.0).clamp(0.0, 1.0)) * a
    };

    // Crop to the glyph's (square, centered) bounding box: the tile has a lot
    // of padding and the glyph would come out tiny at menubar size.
    let (mut bx0, mut bx1, mut by0, mut by1) = (w, 0usize, h, 0usize);
    for y in 0..h {
        for x in 0..w {
            if glyph_alpha(x, y) > 0.5 {
                bx0 = bx0.min(x);
                bx1 = bx1.max(x);
                by0 = by0.min(y);
                by1 = by1.max(y);
            }
        }
    }
    anyhow::ensure!(bx0 <= bx1 && by0 <= by1, "no glyph pixels found in app icon");
    let side = (bx1 - bx0 + 1).max(by1 - by0 + 1);
    let cx = (bx0 + bx1) / 2;
    let cy = (by0 + by1) / 2;
    let ox0 = cx.saturating_sub(side / 2) as isize;
    let oy0 = cy.saturating_sub(side / 2) as isize;

    let out_px = size as usize;
    let mut rgba = vec![0u8; out_px * out_px * 4];
    for oy in 0..out_px {
        let (y0, y1) = (oy * side / out_px, ((oy + 1) * side / out_px).max(oy * side / out_px + 1));
        for ox in 0..out_px {
            let (x0, x1) = (ox * side / out_px, ((ox + 1) * side / out_px).max(ox * side / out_px + 1));
            let mut acc = 0.0;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let (ix, iy) = (ox0 + sx as isize, oy0 + sy as isize);
                    if ix >= 0 && (ix as usize) < w && iy >= 0 && (iy as usize) < h {
                        acc += glyph_alpha(ix as usize, iy as usize);
                    }
                }
            }
            let avg = acc / ((y1 - y0) * (x1 - x0)) as f32;
            let idx = (oy * out_px + ox) * 4;
            rgba[idx + 3] = (avg * 255.0).round() as u8;
        }
    }
    Icon::from_rgba(rgba, size, size).map_err(|e| anyhow::anyhow!("bad rgba: {e}"))
}

fn fallback_idle_icon() -> Icon {
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
