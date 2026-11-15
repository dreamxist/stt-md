use anyhow::Result;
use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use std::path::{Path, PathBuf};
use std::time::Instant;

use stt_md::config::{Config, OutputMode};
use stt_md::pipeline::{self, RecordingInput};

/// Runs the app's own pipeline on existing WAVs against any vault: the way to
/// reprocess an old recording, or to try a change without touching the real
/// vault. Everything but `vault_root` comes from the installed config.
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: test-e2e <mic.wav> [sys.wav] <vault-root>");
        std::process::exit(1);
    }
    let mic_path = PathBuf::from(&args[1]);
    let (sys_path, vault_root) = if args.len() >= 4 {
        (Some(PathBuf::from(&args[2])), PathBuf::from(&args[3]))
    } else {
        (None, PathBuf::from(&args[2]))
    };

    println!("=== test-e2e ===");
    println!("mic wav: {}", mic_path.display());
    if let Some(p) = &sys_path {
        println!("sys wav: {}", p.display());
    }
    println!("vault:   {}\n", vault_root.display());

    let mut cfg = Config::load_or_init()?;
    cfg.output_mode = OutputMode::Obsidian;
    cfg.vault_root = vault_root;
    cfg.validate()?;

    // Reprocessing an old recording should land in its original week and
    // daily note, so take the start time from the `YYYY-MM-DD-HHMMSS` file name.
    let input = RecordingInput {
        started_at: started_at_from_file_name(&mic_path).unwrap_or_else(Local::now),
        mic_path,
        sys_path,
        duration_min: None,
        calendar_title: None,
    };

    let t0 = Instant::now();
    let meeting = pipeline::process(&input, &cfg, &mut |stage| println!("… {}", stage.label()))?;

    println!("\ntitle:    {}", meeting.title);
    println!("model:    {:?}", meeting.model);
    println!("segments: {}", meeting.segments);
    println!("duration: {} min", meeting.duration_min);
    println!("note:     {}", meeting.note_path.display());
    if let Some(e) = &meeting.daily_error {
        println!("daily:    NOT updated — {e}");
    }
    println!("\n✓ E2E test complete in {:.0}s", t0.elapsed().as_secs_f32());
    Ok(())
}

fn started_at_from_file_name(path: &Path) -> Option<DateTime<Local>> {
    let name = path.file_name()?.to_str()?;
    let stamp = name.get(..17)?;
    let naive = NaiveDateTime::parse_from_str(stamp, "%Y-%m-%d-%H%M%S").ok()?;
    Local.from_local_datetime(&naive).single()
}
