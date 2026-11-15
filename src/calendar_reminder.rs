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

/// How far back an event may have started and still be "the meeting that is
/// on": long workshops exist, all-day blocks are excluded elsewhere.
const CURRENT_LOOKBACK_SECS: f64 = 6.0 * 3600.0;
/// A recording started a couple of minutes early still belongs to the event.
const CURRENT_LEAD_SECS: f64 = 2.0 * 60.0;

/// Title of the calendar meeting happening right now, kept fresh by the
/// calendar thread. A recording that starts takes it along as context for the
/// summary and as `event:` in the note.
pub type CurrentEvent = Arc<Mutex<Option<String>>>;

/// Starts the calendar thread: it nudges when a meeting starts and nothing is
/// recording, and keeps `current` pointing at the meeting that is on.
pub fn spawn(state: Arc<Mutex<AppState>>, current: CurrentEvent) {
    let _ = thread::Builder::new()
        .name("calendar-reminder".into())
        .spawn(move || run_loop(state, current));
}

fn run_loop(state: Arc<Mutex<AppState>>, current: CurrentEvent) {
    let store = unsafe { EKEventStore::new() };
    request_access_if_needed(&store);

    // Eventos ya resueltos (avisados, o que se estaban grabando). Clave con
    // la hora de inicio: una serie recurrente comparte identifier.
    let mut handled: HashSet<String> = HashSet::new();

    loop {
        let status = unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) };
        if status == EKAuthorizationStatus::FullAccess {
            let candidates = autoreleasepool(|_| read_events(&store));
            *current.lock() = pick_current(&candidates).map(|c| c.title.trim().to_string());
            remind_of_starting(&candidates, &state, &mut handled);
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

/// Every event overlapping the window that matters: from those that began
/// hours ago and may still be on, to those about to start.
fn read_events(store: &EKEventStore) -> Vec<Candidate> {
    let start = NSDate::dateWithTimeIntervalSinceNow(-CURRENT_LOOKBACK_SECS);
    let end = NSDate::dateWithTimeIntervalSinceNow(CURRENT_LEAD_SECS.max(LEAD_SECS));
    let predicate =
        unsafe { store.predicateForEventsWithStartDate_endDate_calendars(&start, &end, None) };
    let events = unsafe { store.eventsMatchingPredicate(&predicate) };
    events.iter().map(|event| read_event(&event)).collect()
}

/// The meeting that is on right now: started (or about to) and not over. With
/// back-to-back or overlapping events, the one that started last.
fn pick_current(candidates: &[Candidate]) -> Option<&Candidate> {
    candidates
        .iter()
        .filter(|c| {
            c.looks_like_meeting()
                && !c.title.trim().is_empty()
                && c.starts_in_secs <= CURRENT_LEAD_SECS
                && c.ends_in_secs > 0.0
        })
        .min_by(|a, b| a.starts_in_secs.abs().total_cmp(&b.starts_in_secs.abs()))
}

fn remind_of_starting(
    candidates: &[Candidate],
    state: &Mutex<AppState>,
    handled: &mut HashSet<String>,
) {
    for candidate in candidates {
        if !(-GRACE_SECS..=LEAD_SECS).contains(&candidate.starts_in_secs)
            || !candidate.looks_like_meeting()
            || handled.contains(&candidate.key)
        {
            continue;
        }
        if !state.lock().is_recording() {
            println!("[stt-md] calendar meeting starting: {}", candidate.title);
            notifications::calendar_meeting_starting(&candidate.title);
        }
        handled.insert(candidate.key.clone());
    }
}

struct Candidate {
    key: String,
    title: String,
    starts_in_secs: f64,
    ends_in_secs: f64,
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
        let ends_in_secs = event.endDate().timeIntervalSinceNow();
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
            ends_in_secs,
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
            ends_in_secs: 1800.0,
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

    fn at(title: &str, starts_in_min: f64, ends_in_min: f64) -> Candidate {
        Candidate {
            title: title.into(),
            starts_in_secs: starts_in_min * 60.0,
            ends_in_secs: ends_in_min * 60.0,
            ..candidate(1, "", false)
        }
    }

    #[test]
    fn current_event_is_the_meeting_that_is_on_now() {
        let events = [
            at("Ya terminó", -90.0, -30.0),
            at("Workshop largo", -120.0, 60.0),
            at("Coordinación semanal", -3.0, 27.0),
            at("La siguiente", 30.0, 60.0),
            Candidate { all_day: true, ..at("Feriado", -600.0, 600.0) },
            Candidate { other_attendees: 0, ..at("Foco", -1.0, 59.0) },
        ];
        assert_eq!(pick_current(&events).map(|c| c.title.as_str()), Some("Coordinación semanal"));

        // Grabar un minuto antes de la hora igual toma el evento que viene.
        let early = [at("Standup", 1.0, 16.0)];
        assert_eq!(pick_current(&early).map(|c| c.title.as_str()), Some("Standup"));

        assert!(pick_current(&[at("Más tarde", 30.0, 60.0)]).is_none());
        assert!(pick_current(&[at("  ", -1.0, 30.0)]).is_none());
    }

    #[test]
    fn all_day_events_are_never_meetings() {
        assert!(!candidate(3, "https://meet.google.com/x", true).looks_like_meeting());
    }
}
