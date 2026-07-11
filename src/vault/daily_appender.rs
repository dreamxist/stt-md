use anyhow::Result;
use chrono::{DateTime, Local};
use std::fs;
use std::path::{Path, PathBuf};

use super::meeting_writer::day_name_es;

const AGENT_LOG_HEADER: &str = "## 🤖 Agent Log";

/// Appends a meeting link line into the daily note's `## 🤖 Agent Log` section.
/// `daily_note_rel` is the vault-relative path of the daily (already expanded
/// from the config pattern). Creates the daily file (with minimal frontmatter)
/// if it doesn't exist; adds the Agent Log section if missing.
pub fn append_meeting_link(
    vault_root: &Path,
    daily_note_rel: &str,
    daily_template: Option<&str>,
    meeting_started_at: DateTime<Local>,
    meeting_title: &str,
    meeting_stem: &str,
    duration_min: i64,
) -> Result<PathBuf> {
    let date_str = meeting_started_at.format("%Y-%m-%d").to_string();
    let time_str = meeting_started_at.format("%H:%M").to_string();

    let daily_path = vault_root.join(daily_note_rel);
    if let Some(parent) = daily_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let line = format!(
        "- {time_str} — [[{meeting_stem}|{meeting_title}]] ({duration_min}m) — `stt-md`"
    );

    let mut existing = fs::read_to_string(&daily_path).unwrap_or_default();
    // Daily doesn't exist yet: instantiate the user's template (if configured)
    // so we don't block Obsidian's template flow with a minimal stub.
    if existing.trim().is_empty()
        && let Some(tpl) = daily_template
    {
        existing = instantiate_template(tpl, &meeting_started_at);
    }
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

/// Minimal Obsidian-template instantiation: supports the `{{date:YYYY-MM-DD}}`
/// and `{{date:dddd}}` (día en español) moustaches. Anything else is left as-is.
fn instantiate_template(template: &str, dt: &DateTime<Local>) -> String {
    template
        .replace("{{date:YYYY-MM-DD}}", &dt.format("%Y-%m-%d").to_string())
        .replace("{{date:dddd}}", day_name_es(dt))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_appends_inside_existing_section() {
        let existing = "# Daily\n\n## 🤖 Agent Log\n\n- 10:00 — old\n\n## Notas\n\ntexto\n";
        let out = insert_into_section(
            existing,
            existing.find(AGENT_LOG_HEADER).unwrap(),
            AGENT_LOG_HEADER,
            "- 11:00 — new",
        );
        let log_start = out.find(AGENT_LOG_HEADER).unwrap();
        let notas_start = out.find("## Notas").unwrap();
        let section = &out[log_start..notas_start];
        assert!(section.contains("- 10:00 — old"));
        assert!(section.contains("- 11:00 — new"));
        assert!(out.ends_with("texto\n"));
    }

    #[test]
    fn insert_appends_at_end_when_last_section() {
        let existing = "## 🤖 Agent Log\n\n- 10:00 — old";
        let out = insert_into_section(existing, 0, AGENT_LOG_HEADER, "- 11:00 — new");
        assert!(out.ends_with("- 10:00 — old\n- 11:00 — new\n"));
    }

    #[test]
    fn append_creates_daily_and_then_appends() {
        let dir = std::env::temp_dir().join(format!("stt-md-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let now = Local::now();
        let daily_rel = "journey/semana-x/hoy.md";
        let p1 =
            append_meeting_link(&dir, daily_rel, None, now, "Reunión A", "2026-07-11-1000-a", 30)
                .unwrap();
        let p2 =
            append_meeting_link(&dir, daily_rel, None, now, "Reunión B", "2026-07-11-1100-b", 15)
                .unwrap();
        assert_eq!(p1, p2);
        assert_eq!(p1, dir.join(daily_rel));
        let content = std::fs::read_to_string(&p1).unwrap();
        assert!(content.starts_with("---\n"));
        assert_eq!(content.matches(AGENT_LOG_HEADER).count(), 1);
        assert!(content.contains("[[2026-07-11-1000-a|Reunión A]] (30m)"));
        assert!(content.contains("[[2026-07-11-1100-b|Reunión B]] (15m)"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_daily_is_created_from_template() {
        let dir = std::env::temp_dir().join(format!("stt-md-tpl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let tpl = "---\ndate: {{date:YYYY-MM-DD}}\nday: {{date:dddd}}\ntags: [daily]\n---\n\n\
                   # {{date:YYYY-MM-DD}}\n\n## 🎯 Top 3 hoy\n- [ ]\n\n## 🤖 Agent Log\n\n\
                   ## 🔄 Mañana\n";
        let now = Local::now();
        let p = append_meeting_link(&dir, "hoy.md", Some(tpl), now, "Reu", "2026-07-11-1000-reu", 5)
            .unwrap();
        let content = std::fs::read_to_string(&p).unwrap();
        let date = now.format("%Y-%m-%d").to_string();
        assert!(content.contains(&format!("date: {date}")));
        assert!(!content.contains("{{date"));
        assert!(content.contains("## 🎯 Top 3 hoy"));
        // Link landed inside Agent Log, before the next section.
        let log = content.find("## 🤖 Agent Log").unwrap();
        let manana = content.find("## 🔄 Mañana").unwrap();
        let link = content.find("[[2026-07-11-1000-reu|Reu]] (5m)").unwrap();
        assert!(log < link && link < manana);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
