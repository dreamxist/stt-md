pub mod whisper;

/// Who spoke a segment: the local user (mic track) or the remote side
/// (system-audio track). Absent for single-track recordings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    Me,
    Them,
}

impl Speaker {
    pub fn label(self) -> &'static str {
        match self {
            Speaker::Me => "yo",
            Speaker::Them => "ellos",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TranscriptSegment {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    pub speaker: Option<Speaker>,
}

impl TranscriptSegment {
    pub fn duration_ms(&self) -> i64 {
        self.end_ms - self.start_ms
    }
}

/// Interleave the mic and system-audio transcripts into one timeline, tagging
/// each segment with its speaker. Stable sort keeps same-timestamp segments in
/// mic-then-system order.
pub fn merge_segments(
    mic: Vec<TranscriptSegment>,
    sys: Vec<TranscriptSegment>,
) -> Vec<TranscriptSegment> {
    let mut merged: Vec<TranscriptSegment> = mic
        .into_iter()
        .map(|mut s| {
            s.speaker = Some(Speaker::Me);
            s
        })
        .chain(sys.into_iter().map(|mut s| {
            s.speaker = Some(Speaker::Them);
            s
        }))
        .collect();
    merged.sort_by_key(|s| s.start_ms);
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start_ms: i64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            start_ms,
            end_ms: start_ms + 1000,
            text: text.to_string(),
            speaker: None,
        }
    }

    #[test]
    fn merge_interleaves_by_start_time() {
        let mic = vec![seg(0, "hola"), seg(5000, "de acuerdo")];
        let sys = vec![seg(2000, "buenas"), seg(7000, "perfecto")];
        let merged = merge_segments(mic, sys);
        let texts: Vec<&str> = merged.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["hola", "buenas", "de acuerdo", "perfecto"]);
        assert_eq!(merged[0].speaker, Some(Speaker::Me));
        assert_eq!(merged[1].speaker, Some(Speaker::Them));
    }

    #[test]
    fn merge_labels_all_segments() {
        let merged = merge_segments(vec![seg(0, "a")], vec![seg(1, "b")]);
        assert!(merged.iter().all(|s| s.speaker.is_some()));
    }

    #[test]
    fn merge_handles_empty_sides() {
        let only_mic = merge_segments(vec![seg(0, "a")], vec![]);
        assert_eq!(only_mic.len(), 1);
        assert_eq!(only_mic[0].speaker, Some(Speaker::Me));

        let only_sys = merge_segments(vec![], vec![seg(0, "b")]);
        assert_eq!(only_sys.len(), 1);
        assert_eq!(only_sys[0].speaker, Some(Speaker::Them));

        assert!(merge_segments(vec![], vec![]).is_empty());
    }
}
