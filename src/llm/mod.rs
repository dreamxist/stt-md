pub mod ollama;
pub mod prompts;

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::vault::scanner::VaultVocabulary;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionItem {
    #[serde(default)]
    pub who: Option<String>,
    pub task: String,
    #[serde(default)]
    pub deadline: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingSummary {
    pub title: String,
    pub summary_md: String,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub action_items: Vec<ActionItem>,
    #[serde(default)]
    pub people: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub project_wikilink: Option<String>,
}

impl MeetingSummary {
    /// Drop hallucinated tags and wikilinks that don't actually exist in the vault.
    /// LLMs invent things; this is the cheap, deterministic safety net.
    pub fn enforce_vocab(&mut self, vocab: &VaultVocabulary) {
        let valid_tags: HashSet<&String> = vocab
            .frontmatter_tags
            .iter()
            .chain(vocab.inline_tags.iter())
            .collect();
        self.tags.retain(|t| valid_tags.contains(t));
        if !self.tags.iter().any(|t| t == "meeting") {
            self.tags.push("meeting".to_string());
        }

        if let Some(link) = &self.project_wikilink {
            let cleaned = link.trim_start_matches("[[").trim_end_matches("]]");
            if !vocab.wikilink_targets.contains(cleaned) {
                self.project_wikilink = None;
            }
        }

        // Normalize person names to ASCII kebab-case so wikilinks are clean.
        for p in &mut self.people {
            *p = normalize_person_name(p);
        }
        self.people.retain(|p| !p.is_empty());
        self.people.sort();
        self.people.dedup();

        // Validate deadlines: keep only YYYY-MM-DD; drop free text like "jueves próximo".
        for item in &mut self.action_items {
            if let Some(d) = &item.deadline {
                if !is_valid_iso_date(d) {
                    item.deadline = None;
                }
            }
            if let Some(who) = &item.who {
                let n = normalize_person_name(who);
                item.who = if n.is_empty() { None } else { Some(n) };
            }
        }
    }
}

fn normalize_person_name(s: &str) -> String {
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

fn is_valid_iso_date(s: &str) -> bool {
    // Accept "YYYY-MM-DD" only.
    let bytes = s.as_bytes();
    if bytes.len() != 10 {
        return false;
    }
    bytes
        .iter()
        .enumerate()
        .all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            _ => b.is_ascii_digit(),
        })
}
