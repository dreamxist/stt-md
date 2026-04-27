use anyhow::Result;
use chrono::{DateTime, Local};
use std::fs;
use std::path::{Path, PathBuf};

use super::meeting_writer::day_name_es;

const AGENT_LOG_HEADER: &str = "## 🤖 Agent Log";

/// Appends a meeting link line into the daily note's `## 🤖 Agent Log` section.
/// Creates the daily file (with minimal frontmatter) if it doesn't exist.
/// Adds the Agent Log section if the daily exists but doesn't have it yet.
pub fn append_meeting_link(
    vault_root: &Path,
    meeting_started_at: DateTime<Local>,
    meeting_title: &str,
    meeting_vault_relative: &str,
    duration_min: i64,
) -> Result<PathBuf> {
    let year = meeting_started_at.format("%Y").to_string();
    let month = meeting_started_at.format("%m").to_string();
    let date_str = meeting_started_at.format("%Y-%m-%d").to_string();
    let time_str = meeting_started_at.format("%H:%M").to_string();

    let daily_dir = vault_root.join("2-calendar").join(&year).join(&month);
    fs::create_dir_all(&daily_dir)?;
    let daily_path = daily_dir.join(format!("{date_str}.md"));

    let line = format!(
        "- {time_str} — [[{meeting_vault_relative}|{meeting_title}]] ({duration_min}m) — `stt-md`"
    );

    let existing = fs::read_to_string(&daily_path).unwrap_or_default();
    let new_content = if existing.trim().is_empty() {
        format!(
            "---\ndate: {date_str}\nday: {}\ntags: [daily]\n---\n\n{AGENT_LOG_HEADER}\n\n{line}\n",
            day_name_es(&meeting_started_at)
        )
    } else if let Some(idx) = existing.find(AGENT_LOG_HEADER) {
        insert_into_section(&existing, idx, AGENT_LOG_HEADER, &line)
    } else {
        let mut s = existing;
        if !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str(&format!("\n{AGENT_LOG_HEADER}\n\n{line}\n"));
        s
    };

    fs::write(&daily_path, new_content)?;
    Ok(daily_path)
}

fn insert_into_section(existing: &str, header_idx: usize, header: &str, new_line: &str) -> String {
    let after_header_start = header_idx + header.len();
    let after = &existing[after_header_start..];
    let next_heading_offset = after.find("\n## ").map(|i| after_header_start + i);

    let mut out = String::with_capacity(existing.len() + new_line.len() + 2);
    match next_heading_offset {
        Some(end) => {
            out.push_str(&existing[..end]);
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(new_line);
            out.push('\n');
            out.push_str(&existing[end..]);
        }
        None => {
            out.push_str(existing);
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(new_line);
            out.push('\n');
        }
    }
    out
}
