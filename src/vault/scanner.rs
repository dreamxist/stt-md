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
