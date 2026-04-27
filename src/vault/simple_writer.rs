use anyhow::Result;
use chrono::{DateTime, Local};
use std::fs;
use std::path::{Path, PathBuf};

use crate::llm::MeetingSummary;
use crate::transcription::TranscriptSegment;

use super::meeting_writer::slugify;

/// Plain-Markdown writer (used when `output_mode = "simple"`).
/// Writes to `<output_dir>/YYYY-MM-DD-HHMM-slug.md` with no wikilinks,
/// no nested folders, no daily appender.
pub fn write_simple(
    output_dir: &Path,
    started_at: DateTime<Local>,
    summary: &MeetingSummary,
    segments: &[TranscriptSegment],
    duration_min: i64,
    audio_path: &Path,
) -> Result<PathBuf> {
    fs::create_dir_all(output_dir)?;
    let slug = slugify(&summary.title);
    let stem = format!("{}-{}", started_at.format("%Y-%m-%d-%H%M"), slug);
    let path = output_dir.join(format!("{stem}.md"));

    let audio_filename = audio_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut tags: Vec<String> = summary.tags.iter().filter(|t| *t != "meeting").cloned().collect();
    tags.insert(0, "meeting".to_string());
    let tags_yaml = format!("[{}]", tags.join(", "));
    let people_yaml = if summary.people.is_empty() {
        "[]".to_string()
    } else {
        format!("[{}]", summary.people.join(", "))
    };

    let mut body = String::new();
    body.push_str("---\n");
    body.push_str(&format!("date: {}\n", started_at.format("%Y-%m-%d")));
    body.push_str(&format!("time: {}\n", started_at.format("%H:%M")));
    body.push_str(&format!("title: {}\n", summary.title));
    body.push_str(&format!("duration_min: {duration_min}\n"));
    body.push_str(&format!("tags: {tags_yaml}\n"));
    body.push_str(&format!("people: {people_yaml}\n"));
    if !audio_filename.is_empty() {
        body.push_str(&format!("audio: {audio_filename}\n"));
    }
    body.push_str("source: stt-md\n");
    body.push_str("---\n\n");

    body.push_str(&format!("# {}\n\n", summary.title));

    body.push_str("## Resumen\n\n");
    body.push_str(summary.summary_md.trim());
    body.push_str("\n\n");

    if !summary.decisions.is_empty() {
        body.push_str("## Decisiones\n\n");
        for d in &summary.decisions {
            body.push_str(&format!("- {d}\n"));
        }
        body.push('\n');
    }

    if !summary.action_items.is_empty() {
        body.push_str("## Action items\n\n");
        for a in &summary.action_items {
            let who = a
                .who
                .as_deref()
                .map(|w| format!("@{w} — "))
                .unwrap_or_default();
            let deadline = a
                .deadline
                .as_deref()
                .map(|d| format!(" *(deadline: {d})*"))
                .unwrap_or_default();
            body.push_str(&format!("- [ ] {who}{}{deadline}\n", a.task));
        }
        body.push('\n');
    }

    if !summary.people.is_empty() {
        body.push_str("## Personas\n\n");
        for p in &summary.people {
            body.push_str(&format!("- {p}\n"));
        }
        body.push('\n');
    }

    body.push_str("## Transcripción\n\n");
    body.push_str("<details>\n<summary>Ver transcripción completa</summary>\n\n");
    for s in segments {
        let mins = s.start_ms / 60_000;
        let secs = (s.start_ms % 60_000) / 1000;
        body.push_str(&format!("[{:02}:{:02}] {}\n", mins, secs, s.text));
    }
    body.push_str("\n</details>\n");

    fs::write(&path, body)?;
    Ok(path)
}
