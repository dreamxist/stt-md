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
///
/// A call played through speakers reaches the mic too, so the mic track
/// carries a second, slightly different copy of everything the other side
/// said. Left in, it doubles the transcript (a 76-minute call overflowed the
/// summarizer's budget and lost its middle) and pins their words on "yo".
/// The system track has no echo, so it is the reference: a mic segment whose
/// words are mostly said by the system track at the same moment is dropped.
pub fn merge_segments(
    mic: Vec<TranscriptSegment>,
    sys: Vec<TranscriptSegment>,
) -> Vec<TranscriptSegment> {
    let mic: Vec<TranscriptSegment> = mic.into_iter().filter(|m| !is_echo(m, &sys)).collect();
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

/// How far apart the two tracks' timestamps can drift for the same words:
/// each track is cut into windows on its own, so segment boundaries differ.
const ECHO_SLACK_MS: i64 = 5_000;
/// Share of a mic segment's words that must appear in the overlapping system
/// segments for it to count as echo. Below this the mic segment mixes in
/// words of its own and is kept whole.
const ECHO_WORD_SHARE: f32 = 0.6;

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn is_echo(mic: &TranscriptSegment, sys: &[TranscriptSegment]) -> bool {
    let mic_words = words(&mic.text);
    if mic_words.is_empty() {
        return false;
    }
    let nearby: Vec<&TranscriptSegment> = sys
        .iter()
        .filter(|s| s.start_ms <= mic.end_ms + ECHO_SLACK_MS && s.end_ms + ECHO_SLACK_MS >= mic.start_ms)
        .collect();
    // A short "sí" or "ya" is said by both sides all the time; only call it
    // echo when the other track has the exact same utterance.
    if mic_words.len() < 3 {
        return nearby.iter().any(|s| words(&s.text) == mic_words);
    }
    let heard: std::collections::HashSet<String> =
        nearby.iter().flat_map(|s| words(&s.text)).collect();
    let shared = mic_words.iter().filter(|w| heard.contains(*w)).count();
    shared as f32 >= mic_words.len() as f32 * ECHO_WORD_SHARE
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

    fn at(start_ms: i64, end_ms: i64, text: &str) -> TranscriptSegment {
        TranscriptSegment { start_ms, end_ms, text: text.to_string(), speaker: None }
    }

    #[test]
    fn drops_mic_echo_of_the_other_side() {
        let sys = vec![at(0, 6000, "El martes revisamos el presupuesto del trimestre con el equipo.")];
        let mic = vec![at(800, 6500, "el martes revisamos el presupuesto del trimestre con equipo")];
        let merged = merge_segments(mic, sys);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].speaker, Some(Speaker::Them));
    }

    #[test]
    fn keeps_my_own_words_said_while_they_talk() {
        let sys = vec![at(0, 6000, "El martes revisamos el presupuesto del trimestre.")];
        let mic = vec![at(3000, 7000, "Yo puedo mandar la planilla mañana temprano.")];
        assert_eq!(merge_segments(mic, sys).len(), 2);
    }

    #[test]
    fn keeps_the_same_words_said_much_later() {
        let sys = vec![at(0, 5000, "Revisamos el presupuesto del trimestre.")];
        let mic = vec![at(60_000, 65_000, "Revisamos el presupuesto del trimestre.")];
        assert_eq!(merge_segments(mic, sys).len(), 2);
    }

    #[test]
    fn short_replies_only_drop_on_an_exact_match() {
        let sys = vec![at(0, 1000, "Ya, ya.")];
        assert_eq!(merge_segments(vec![at(200, 1200, "ya ya")], sys.clone()).len(), 1);
        assert_eq!(merge_segments(vec![at(200, 1200, "Sí.")], sys).len(), 2);
    }
}
