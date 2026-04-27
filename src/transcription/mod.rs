pub mod whisper;

#[derive(Debug, Clone)]
pub struct TranscriptSegment {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

impl TranscriptSegment {
    pub fn duration_ms(&self) -> i64 {
        self.end_ms - self.start_ms
    }
}
