use anyhow::Result;
use chrono::{DateTime, Datelike, Local};
use std::fs;
use std::path::{Path, PathBuf};

use crate::llm::MeetingSummary;
use crate::transcription::TranscriptSegment;

/// Writes the full meeting markdown into `<vault>/2-calendar/YYYY/MM/meetings/`.
/// Returns both the absolute path and the vault-relative path (used by the
/// daily appender to build the wikilink).
pub struct WrittenMeeting {
    pub absolute_path: PathBuf,
    pub vault_relative: String,
}

pub fn write_meeting(
    vault_root: &Path,
    started_at: DateTime<Local>,
    summary: &MeetingSummary,
    segments: &[TranscriptSegment],
    duration_min: i64,
    audio_path: &Path,
) -> Result<WrittenMeeting> {
    let slug = slugify(&summary.title);
    let year = started_at.format("%Y").to_string();
    let month = started_at.format("%m").to_string();
    let meetings_dir = vault_root
        .join("2-calendar")
        .join(&year)
        .join(&month)
        .join("meetings");
    fs::create_dir_all(&meetings_dir)?;

    let stem = format!("{}-{}", started_at.format("%Y-%m-%d-%H%M"), slug);
    let filename = format!("{stem}.md");
    let path = meetings_dir.join(&filename);
    let vault_relative = format!("2-calendar/{year}/{month}/meetings/{stem}");

    let audio_filename = audio_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut body = String::new();
    body.push_str("---\n");
    body.push_str(&format!("date: {}\n", started_at.format("%Y-%m-%d")));
    body.push_str(&format!("day: {}\n", day_name_es(&started_at)));
    body.push_str(&format!("time: {}\n", started_at.format("%H:%M")));
    body.push_str(&format!("title: {}\n", summary.title));
    body.push_str(&format!("duration_min: {duration_min}\n"));
    body.push_str(&format!("tags: {}\n", format_tags_yaml(&summary.tags)));
    body.push_str(&format!("people: {}\n", format_list_yaml(&summary.people)));
    if let Some(link) = &summary.project_wikilink {
        body.push_str(&format!("project: \"{link}\"\n"));
    }
    if !audio_filename.is_empty() {
        body.push_str(&format!("audio: {audio_filename}\n"));
    }
    body.push_str("type: meeting\n");
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
            body.push_str(&format!("- [[{p}]]\n"));
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

    Ok(WrittenMeeting {
        absolute_path: path,
        vault_relative,
    })
}

fn format_tags_yaml(tags: &[String]) -> String {
    let mut all: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();
    if !all.iter().any(|t| *t == "meeting") {
        all.insert(0, "meeting");
    }
    if !all.iter().any(|t| *t == "ai-draft") {
        // Mark drafts so the user knows they're auto-generated.
        let pos = all.iter().position(|t| *t == "meeting").map(|i| i + 1).unwrap_or(0);
        all.insert(pos, "ai-draft");
    }
    format!("[{}]", all.join(", "))
}

fn format_list_yaml(items: &[String]) -> String {
    if items.is_empty() {
        "[]".to_string()
    } else {
        format!("[{}]", items.join(", "))
    }
}

pub fn day_name_es(dt: &DateTime<Local>) -> &'static str {
    match dt.weekday() {
        chrono::Weekday::Mon => "lunes",
        chrono::Weekday::Tue => "martes",
        chrono::Weekday::Wed => "miércoles",
        chrono::Weekday::Thu => "jueves",
        chrono::Weekday::Fri => "viernes",
        chrono::Weekday::Sat => "sábado",
        chrono::Weekday::Sun => "domingo",
    }
}

pub fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = true;
    for ch in s.chars() {
        let normalized = match ch {
            'á' | 'à' | 'ä' | 'â' | 'Á' | 'À' | 'Ä' | 'Â' => 'a',
            'é' | 'è' | 'ë' | 'ê' | 'É' | 'È' | 'Ë' | 'Ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' | 'Í' | 'Ì' | 'Ï' | 'Î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'Ó' | 'Ò' | 'Ö' | 'Ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' | 'Ú' | 'Ù' | 'Ü' | 'Û' => 'u',
            'ñ' | 'Ñ' => 'n',
            c => c,
        };
        if normalized.is_ascii_alphanumeric() {
            out.push(normalized.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

// Keep backward-compat with phase-3 transcribe-wav binary.
pub fn write_basic_md(
    output_dir: &Path,
    title: &str,
    started_at: DateTime<Local>,
    segments: &[TranscriptSegment],
    audio_path: &Path,
) -> Result<PathBuf> {
    fs::create_dir_all(output_dir)?;
    let slug = slugify(title);
    let filename = format!(
        "{}-{}.md",
        started_at.format("%Y-%m-%d-%H%M"),
        slug
    );
    let path = output_dir.join(&filename);

    let audio_filename = audio_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut body = String::new();
    body.push_str("---\n");
    body.push_str(&format!("date: {}\n", started_at.format("%Y-%m-%d")));
    body.push_str(&format!("time: {}\n", started_at.format("%H:%M")));
    body.push_str(&format!("title: {title}\n"));
    body.push_str(&format!("audio: {audio_filename}\n"));
    body.push_str("tags: [meeting, ai-draft]\n");
    body.push_str("source: stt-md\n");
    body.push_str("---\n\n");
    body.push_str(&format!("# {title}\n\n"));
    body.push_str("## Transcripción\n\n");
    for s in segments {
        let mins = s.start_ms / 60_000;
        let secs = (s.start_ms % 60_000) / 1000;
        body.push_str(&format!("[{:02}:{:02}] {}\n", mins, secs, s.text));
    }

    fs::write(&path, body)?;
    Ok(path)
}
