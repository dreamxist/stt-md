use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AppState {
    Idle,
    Recording { started_at: Instant },
    Processing,
}

impl AppState {
    pub fn is_recording(&self) -> bool {
        matches!(self, AppState::Recording { .. })
    }
}
