use anyhow::Result;
use std::collections::BTreeSet;
use std::path::Path;
use walkdir::WalkDir;

#[derive(Debug, Clone, Default)]
pub struct VaultVocabulary {
    pub frontmatter_tags: BTreeSet<String>,
    pub inline_tags: BTreeSet<String>,
    pub wikilink_targets: BTreeSet<String>,
}

impl VaultVocabulary {
    pub fn all_tags(&self) -> Vec<String> {
        self.frontmatter_tags
            .iter()
            .chain(self.inline_tags.iter())
            .cloned()
            .collect()
    }
}

pub fn scan_vault(root: &Path) -> Result<VaultVocabulary> {
    let mut vocab = VaultVocabulary::default();

    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !is_excluded(e.path()))
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            vocab.wikilink_targets.insert(stem.to_string());
        }

        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };

        if let Some(fm) = extract_frontmatter(&content) {
            for tag in extract_tags_from_frontmatter(fm) {
                vocab.frontmatter_tags.insert(tag);
            }
        }

        for tag in extract_inline_tags(&content) {
            vocab.inline_tags.insert(tag);
        }
    }

    Ok(vocab)
}

/// Lists area paths (relative to `areas_dir`, e.g. "work", "work/acmecorp")
/// walking up to two levels of subfolders. Used as a closed list so the LLM
/// can attribute a meeting to an existing area but never invent one.
pub fn scan_areas(vault_root: &Path, areas_dir: &str) -> Vec<String> {
    let root = vault_root.join(areas_dir);
    let mut areas = Vec::new();
    for entry in WalkDir::new(&root)
        .min_depth(1)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_dir() {
            continue;
        }
        if entry
            .file_name()
            .to_str()
            .map(|n| n.starts_with('.') || n.starts_with('_'))
            .unwrap_or(true)
        {
            continue;
        }
        if let Ok(rel) = entry.path().strip_prefix(&root) {
            areas.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    areas.sort();
    areas
}

fn is_excluded(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("/.git")
        || s.contains("/.obsidian")
        || s.contains("/.smart-env")
        || s.contains("/.smart-connections")
        || s.contains("/5-ai-log/sessions")
        || s.contains("/9-archive")
}

fn extract_frontmatter(content: &str) -> Option<&str> {
    let body = content.strip_prefix("---\n")?;
    let end = body.find("\n---")?;
    Some(&body[..end])
}

fn extract_tags_from_frontmatter(fm: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let mut in_tags_block = false;
    for line in fm.lines() {
        let trimmed = line.trim_end();
        if let Some(rest) = trimmed.trim_start().strip_prefix("tags:") {
            in_tags_block = false;
            let rest = rest.trim();
            if let Some(inner) = rest.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                for raw in inner.split(',') {
                    push_clean_tag(&mut tags, raw);
                }
            } else if rest.is_empty() {
                in_tags_block = true;
            } else {
                push_clean_tag(&mut tags, rest);
            }
        } else if in_tags_block {
            if let Some(rest) = trimmed.trim_start().strip_prefix("- ") {
                push_clean_tag(&mut tags, rest);
            } else if !trimmed.trim_start().starts_with('-')
                && !trimmed.trim().is_empty()
                && !trimmed.starts_with(' ')
                && !trimmed.starts_with('\t')
            {
                in_tags_block = false;
            }
        }
    }
    tags
}

fn push_clean_tag(out: &mut Vec<String>, raw: &str) {
    let cleaned = raw.trim().trim_matches(['"', '\'', '#'].as_ref());
    if cleaned.is_empty() {
        return;
    }
    out.push(cleaned.to_string());
}

fn extract_inline_tags(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' {
            if i > 0 {
                let prev = bytes[i - 1];
                if prev.is_ascii_alphanumeric() {
                    i += 1;
                    continue;
                }
            }
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() {
                let c = bytes[j];
                if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'/' {
                    j += 1;
                } else {
                    break;
                }
            }
            if j > start {
                let tag = &content[start..j];
                if tag
                    .chars()
                    .next()
                    .map(|c| c.is_ascii_lowercase())
                    .unwrap_or(false)
                    && tag.chars().any(|c| c.is_alphabetic())
                {
                    out.push(tag.to_string());
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_extraction() {
        let doc = "---\ntags: [a, b]\ntitle: x\n---\nbody";
        assert_eq!(extract_frontmatter(doc), Some("tags: [a, b]\ntitle: x"));
        assert_eq!(extract_frontmatter("no frontmatter"), None);
    }

    #[test]
    fn frontmatter_tags_inline_array() {
        let tags = extract_tags_from_frontmatter("tags: [meeting, \"ai-draft\", #x]");
        assert_eq!(tags, vec!["meeting", "ai-draft", "x"]);
    }

    #[test]
    fn frontmatter_tags_block_list() {
        let fm = "title: x\ntags:\n  - daily\n  - review\nauthor: y";
        let tags = extract_tags_from_frontmatter(fm);
        assert_eq!(tags, vec!["daily", "review"]);
    }

    #[test]
    fn inline_tags_basic() {
        let tags = extract_inline_tags("hola #standup y #proyecto-x pero no#esto ni #123 ni # solo");
        assert_eq!(tags, vec!["standup", "proyecto-x"]);
    }

    #[test]
    fn inline_tags_ignore_uppercase_and_anchors() {
        let tags = extract_inline_tags("issue #42, #Titulo, #ok");
        assert_eq!(tags, vec!["ok"]);
    }
}

#[cfg(test)]
mod area_tests {
    use super::*;

    #[test]
    fn scan_areas_lists_two_levels_and_skips_hidden() {
        let root = std::env::temp_dir().join(format!("stt-md-areas-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in [
            "areas/personal/entrenamiento",
            "areas/work/acmecorp",
            "areas/work/empresa",
            "areas/.oculta",
            "areas/_meta",
            "areas/work/acmecorp/muy/profundo",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let areas = scan_areas(&root, "areas");
        assert_eq!(
            areas,
            vec![
                "personal",
                "personal/entrenamiento",
                "work",
                "work/empresa",
                "work/acmecorp",
            ]
        );
        // Missing areas dir → empty list, no error.
        assert!(scan_areas(&root, "no-existe").is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
