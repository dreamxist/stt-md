use std::time::Instant;

/// What the recorder is doing. Processing a finished meeting is not a state
/// of the recorder any more: it happens in the background (see `jobs`), so a
/// new recording can start while the previous one is being transcribed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AppState {
    Idle,
    Recording { started_at: Instant },
}

impl AppState {
    pub fn is_recording(&self) -> bool {
        matches!(self, AppState::Recording { .. })
    }
}
