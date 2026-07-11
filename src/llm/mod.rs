pub mod ollama;
pub mod prompts;

use std::collections::{HashMap, HashSet};

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
    /// Vault area this meeting belongs to (e.g. "work/acmecorp"), chosen by
    /// the LLM from the closed list scanned from `areas_dir`. Obsidian mode only.
    #[serde(default)]
    pub area: Option<String>,
}

impl MeetingSummary {
    /// Light cleanup for the simple-mode flow (no vault to enforce against).
    /// Just normalizes person names + drops invalid deadline strings, and
    /// caps the LLM's free-form tags to a sane lowercase kebab-case set.
    pub fn normalize_simple(&mut self, max_tags: usize) {
        for p in &mut self.people {
            *p = normalize_person_name(p);
        }
        self.people.retain(|p| !p.is_empty());
        self.people.sort();
        self.people.dedup();

        for item in &mut self.action_items {
            if let Some(d) = &item.deadline
                && !is_valid_iso_date(d)
            {
                item.deadline = None;
            }
            if let Some(who) = &item.who {
                let n = normalize_person_name(who);
                item.who = if n.is_empty() { None } else { Some(n) };
            }
        }

        self.tags = self
            .tags
            .iter()
            .map(|t| sanitize_kebab(t))
            .filter(|t| !t.is_empty())
            .collect();
        self.tags.sort();
        self.tags.dedup();
        self.tags.truncate(max_tags);
        self.project_wikilink = None;
        self.area = None;
    }

    /// Keeps `area` only if it matches (case-insensitively) one of the vault's
    /// real areas; emits the canonical casing. Empty list disables the field.
    pub fn enforce_area(&mut self, areas: &[String]) {
        self.area = self.area.take().and_then(|a| {
            let wanted = a.trim().trim_matches('/').to_lowercase();
            areas
                .iter()
                .find(|x| x.to_lowercase() == wanted)
                .cloned()
        });
    }

    /// Drop hallucinated tags and wikilinks that don't actually exist in the vault.
    /// LLMs invent things; this is the cheap, deterministic safety net.
    pub fn enforce_vocab(&mut self, vocab: &VaultVocabulary) {
        // Case-insensitive match, but emit the vault's canonical casing so a
        // "Meeting" reply still maps to the existing "meeting" tag.
        let canonical_tags: HashMap<String, &String> = vocab
            .frontmatter_tags
            .iter()
            .chain(vocab.inline_tags.iter())
            .map(|t| (t.to_lowercase(), t))
            .collect();
        let mut seen = HashSet::new();
        self.tags = self
            .tags
            .iter()
            .filter_map(|t| canonical_tags.get(&t.to_lowercase()).map(|c| (*c).clone()))
            .filter(|t| seen.insert(t.clone()))
            .collect();
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
            if let Some(d) = &item.deadline
                && !is_valid_iso_date(d)
            {
                item.deadline = None;
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

fn sanitize_kebab(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = true;
    for ch in s.chars() {
        let n = match ch {
            'á' | 'à' | 'ä' | 'â' | 'Á' | 'À' | 'Ä' | 'Â' => 'a',
            'é' | 'è' | 'ë' | 'ê' | 'É' | 'È' | 'Ë' | 'Ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' | 'Í' | 'Ì' | 'Ï' | 'Î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'Ó' | 'Ò' | 'Ö' | 'Ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' | 'Ú' | 'Ù' | 'Ü' | 'Û' => 'u',
            'ñ' | 'Ñ' => 'n',
            c => c,
        };
        if n.is_ascii_alphanumeric() {
            out.push(n.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn vocab(fm: &[&str], inline: &[&str], links: &[&str]) -> VaultVocabulary {
        VaultVocabulary {
            frontmatter_tags: fm.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>(),
            inline_tags: inline.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>(),
            wikilink_targets: links.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>(),
        }
    }

    fn summary() -> MeetingSummary {
        MeetingSummary {
            title: "t".into(),
            summary_md: "s".into(),
            decisions: vec![],
            action_items: vec![],
            people: vec![],
            tags: vec![],
            project_wikilink: None,
            area: None,
        }
    }

    #[test]
    fn enforce_area_matches_case_insensitive_and_drops_invented() {
        let areas = vec!["personal".to_string(), "work/acmecorp".to_string()];
        let mut s = summary();
        s.area = Some("Work/AcmeCorp".into());
        s.enforce_area(&areas);
        assert_eq!(s.area, Some("work/acmecorp".into()));

        s.area = Some("finanzas".into());
        s.enforce_area(&areas);
        assert_eq!(s.area, None);

        s.area = Some("/personal/".into());
        s.enforce_area(&areas);
        assert_eq!(s.area, Some("personal".into()));
    }

    #[test]
    fn normalize_person_name_kebabs() {
        assert_eq!(normalize_person_name("María González"), "maria-gonzalez");
        assert_eq!(normalize_person_name("  Ñoño  "), "nono");
        assert_eq!(normalize_person_name("@!#"), "");
    }

    #[test]
    fn iso_date_validation() {
        assert!(is_valid_iso_date("2026-07-11"));
        assert!(!is_valid_iso_date("jueves próximo"));
        assert!(!is_valid_iso_date("2026-7-11"));
        assert!(!is_valid_iso_date("2026-07-11T10:00"));
    }

    #[test]
    fn enforce_vocab_drops_hallucinated_and_canonicalizes_case() {
        let v = vocab(&["acme", "Roadmap"], &["standup"], &["acme"]);
        let mut s = summary();
        s.tags = vec![
            "Acme".into(),   // case-insensitive match → canonical "acme"
            "roadmap".into(),   // matches vault's "Roadmap"
            "invented".into(),  // hallucinated → dropped
            "acme".into(),   // duplicate after canonicalization → deduped
        ];
        s.project_wikilink = Some("[[no-existe]]".into());
        s.enforce_vocab(&v);
        assert_eq!(s.tags, vec!["acme", "Roadmap", "meeting"]);
        assert_eq!(s.project_wikilink, None);
    }

    #[test]
    fn enforce_vocab_keeps_valid_wikilink_and_cleans_items() {
        let v = vocab(&[], &[], &["acme"]);
        let mut s = summary();
        s.project_wikilink = Some("[[acme]]".into());
        s.people = vec!["Juan Pérez".into(), "".into()];
        s.action_items = vec![ActionItem {
            who: Some("María".into()),
            task: "x".into(),
            deadline: Some("la próxima semana".into()),
        }];
        s.enforce_vocab(&v);
        assert_eq!(s.project_wikilink, Some("[[acme]]".into()));
        assert_eq!(s.people, vec!["juan-perez"]);
        assert_eq!(s.action_items[0].who, Some("maria".into()));
        assert_eq!(s.action_items[0].deadline, None);
    }

    #[test]
    fn normalize_simple_caps_and_sanitizes_tags() {
        let mut s = summary();
        s.tags = vec![
            "Diseño UX".into(),
            "planning".into(),
            "retro".into(),
            "standup".into(),
            "extra".into(),
        ];
        s.normalize_simple(4);
        assert_eq!(s.tags.len(), 4);
        assert!(s.tags.contains(&"diseno-ux".to_string()));
        assert_eq!(s.project_wikilink, None);
    }
}
