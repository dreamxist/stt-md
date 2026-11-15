//! The notes this app has written, newest last, one JSON line each.
//!
//! The menu reads it to offer the last few meetings; nothing else keeps track
//! of where a note went once its title came back from the summarizer.

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryEntry {
    pub at: DateTime<Local>,
    pub title: String,
    pub note_path: PathBuf,
    pub duration_min: i64,
    #[serde(default)]
    pub model: Option<String>,
}

pub fn append(entry: &HistoryEntry) {
    append_to(&crate::paths::history_file(), entry);
}

/// The last `n` notes that still exist on disk, newest first.
pub fn recent(n: usize) -> Vec<HistoryEntry> {
    recent_in(&crate::paths::history_file(), n)
}

fn append_to(path: &Path, entry: &HistoryEntry) {
    let Ok(line) = serde_json::to_string(entry) else {
        return;
    };
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| writeln!(f, "{line}"));
}

fn recent_in(path: &Path, n: usize) -> Vec<HistoryEntry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<HistoryEntry>(l).ok())
        .filter(|e| e.note_path.exists())
        .take(n)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_is_newest_first_and_skips_notes_that_were_deleted() {
        let dir = std::env::temp_dir().join(format!("stt-md-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("history.jsonl");
        for name in ["a", "b", "c", "d"] {
            let note = dir.join(format!("{name}.md"));
            std::fs::write(&note, "x").unwrap();
            append_to(
                &file,
                &HistoryEntry {
                    at: Local::now(),
                    title: name.to_string(),
                    note_path: note,
                    duration_min: 5,
                    model: None,
                },
            );
        }
        std::fs::remove_file(dir.join("c.md")).unwrap();
        let titles: Vec<String> = recent_in(&file, 2).into_iter().map(|e| e.title).collect();
        assert_eq!(titles, vec!["d", "b"]);
        assert!(recent_in(&dir.join("missing.jsonl"), 5).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
