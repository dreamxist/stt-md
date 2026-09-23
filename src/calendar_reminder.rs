//! "¿Grabo?" cuando empieza un evento del calendario que parece reunión.
//!
//! Complementa a `meeting_detector`: ése ve cuándo una app de videollamada
//! toma el mic, pero una reunión presencial no toca ninguna app. Lee los
//! calendarios de macOS vía EventKit (Google aparece ahí si la cuenta está
//! agregada en Ajustes → Cuentas de Internet). El permiso se pide una vez;
//! si se niega, el hilo queda inactivo en silencio.

use std::collections::HashSet;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::autoreleasepool;
use objc2::runtime::Bool;
use objc2_event_kit::{EKAuthorizationStatus, EKEntityType, EKEvent, EKEventStore};
use objc2_foundation::{NSDate, NSError};
use parking_lot::Mutex;

use crate::app_state::AppState;
use crate::notifications;

const POLL_INTERVAL: Duration = Duration::from_secs(30);
/// Avisar desde 1 min antes hasta 5 min después del inicio: cubre llegar
/// tarde sin avisar por reuniones que ya van por la mitad.
const LEAD_SECS: f64 = 60.0;
const GRACE_SECS: f64 = 5.0 * 60.0;

const VIDEO_LINK_HINTS: &[&str] = &[
    "meet.google.com",
    "zoom.us",
    "teams.microsoft.com",
    "teams.live.com",
    "webex.com",
    "whereby.com",
    "around.co",
];

pub fn spawn(state: Arc<Mutex<AppState>>) {
    let _ = thread::Builder::new()
        .name("calendar-reminder".into())
        .spawn(move || run_loop(state));
}

fn run_loop(state: Arc<Mutex<AppState>>) {
    let store = unsafe { EKEventStore::new() };
    request_access_if_needed(&store);

    // Eventos ya resueltos (avisados, o que se estaban grabando). Clave con
    // la hora de inicio: una serie recurrente comparte identifier.
    let mut handled: HashSet<String> = HashSet::new();

    loop {
        let status = unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) };
        if status == EKAuthorizationStatus::FullAccess {
            autoreleasepool(|_| check_events(&store, &state, &mut handled));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn request_access_if_needed(store: &EKEventStore) {
    let status = unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) };
    if status != EKAuthorizationStatus::NotDetermined {
        return;
    }
    let block = RcBlock::new(|granted: Bool, _err: *mut NSError| {
        println!("[stt-md] calendar access granted: {}", granted.as_bool());
    });
    unsafe { store.requestFullAccessToEventsWithCompletion(RcBlock::as_ptr(&block)) };
}

fn check_events(store: &EKEventStore, state: &Mutex<AppState>, handled: &mut HashSet<String>) {
    let start = NSDate::dateWithTimeIntervalSinceNow(-GRACE_SECS);
    let end = NSDate::dateWithTimeIntervalSinceNow(LEAD_SECS);
    let predicate =
        unsafe { store.predicateForEventsWithStartDate_endDate_calendars(&start, &end, None) };
    let events = unsafe { store.eventsMatchingPredicate(&predicate) };

    for event in events.iter() {
        let candidate = read_event(&event);
        if !(-GRACE_SECS..=LEAD_SECS).contains(&candidate.starts_in_secs)
            || !candidate.looks_like_meeting()
            || handled.contains(&candidate.key)
        {
            continue;
        }
        match *state.lock() {
            AppState::Recording { .. } => {
                handled.insert(candidate.key);
            }
            // Esperar a que termine el post-proceso de la reunión anterior.
            AppState::Processing => {}
            AppState::Idle => {
                println!("[stt-md] calendar meeting starting: {}", candidate.title);
                notifications::calendar_meeting_starting(&candidate.title);
                handled.insert(candidate.key);
            }
        }
    }
}

struct Candidate {
    key: String,
    title: String,
    starts_in_secs: f64,
    all_day: bool,
    other_attendees: usize,
    text: String,
}

impl Candidate {
    /// Un bloque de foco o un recordatorio personal no es una reunión: hace
    /// falta al menos otra persona invitada o un link de videollamada.
    fn looks_like_meeting(&self) -> bool {
        if self.all_day {
            return false;
        }
        let text = self.text.to_lowercase();
        self.other_attendees > 0 || VIDEO_LINK_HINTS.iter().any(|h| text.contains(h))
    }
}

fn read_event(event: &EKEvent) -> Candidate {
    unsafe {
        let start = event.startDate();
        let starts_in_secs = start.timeIntervalSinceNow();
        let title = event.title().to_string();
        let id = event.eventIdentifier().map(|s| s.to_string()).unwrap_or_else(|| title.clone());
        let other_attendees = event
            .attendees()
            .map(|list| list.iter().filter(|p| !p.isCurrentUser()).count())
            .unwrap_or(0);
        let mut text = String::new();
        if let Some(url) = event.URL().and_then(|u| u.absoluteString()) {
            text.push_str(&url.to_string());
            text.push('\n');
        }
        if let Some(location) = event.location() {
            text.push_str(&location.to_string());
            text.push('\n');
        }
        if let Some(notes) = event.notes() {
            text.push_str(&notes.to_string());
        }
        Candidate {
            key: format!("{id}@{}", start.timeIntervalSince1970() as i64),
            title,
            starts_in_secs,
            all_day: event.isAllDay(),
            other_attendees,
            text,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(other_attendees: usize, text: &str, all_day: bool) -> Candidate {
        Candidate {
            key: "k".into(),
            title: "t".into(),
            starts_in_secs: 0.0,
            all_day,
            other_attendees,
            text: text.into(),
        }
    }

    #[test]
    fn meeting_needs_attendees_or_video_link() {
        assert!(candidate(1, "", false).looks_like_meeting());
        assert!(candidate(0, "https://meet.google.com/abc-defg-hij", false).looks_like_meeting());
        assert!(candidate(0, "Unirse: https://US02WEB.ZOOM.US/j/123", false).looks_like_meeting());
        assert!(!candidate(0, "Foco: escribir propuesta", false).looks_like_meeting());
    }

    #[test]
    fn all_day_events_are_never_meetings() {
        assert!(!candidate(3, "https://meet.google.com/x", true).looks_like_meeting());
    }
}
