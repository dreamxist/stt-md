use std::time::{Duration, Instant};

use parking_lot::Mutex;

fn notify(summary: &str, body: &str) {
    let _ = notify_rust::Notification::new()
        .summary(summary)
        .body(body)
        .appname("stt-md")
        .show();
}

/// How to start a recording, as the reminders phrase it. Set once at launch,
/// when it is known whether the global shortcut could be registered.
static START_HINT: Mutex<String> = Mutex::new(String::new());

pub fn set_start_hint(hint: String) {
    *START_HINT.lock() = hint;
}

fn start_hint() -> String {
    let hint = START_HINT.lock();
    if hint.is_empty() {
        "Click en STT en la menubar → Empezar reunión.".to_string()
    } else {
        hint.clone()
    }
}

pub fn meeting_saved(title: &str, duration_min: i64, model: Option<&str>) {
    let body = match model {
        Some(model) => format!("{title} · {duration_min} min · resumen de {model}"),
        None => format!("{title} · {duration_min} min · sin resumen: la nota trae la transcripción"),
    };
    notify("Reunión guardada", &body);
}

/// The note is fine but its link did not make it into the daily.
pub fn daily_not_updated(err: &str) {
    notify("La nota se guardó, pero no se pudo enlazar en el daily", err);
}

pub fn meeting_skipped(reason: &str) {
    notify("No se creó nota", &format!("No había nada que transcribir: {reason}"));
}

pub fn meeting_failed(label: &str, err: &str) {
    notify(
        &format!("No se pudo procesar la reunión del {label}"),
        &format!("{err}. El audio está a salvo: reintenta desde el menú STT."),
    );
}

pub fn recording_failed(err: &str) {
    notify("No se pudo iniciar la grabación", err);
}

/// Recordings left unfinished by a crash or a forced quit, picked up at launch.
pub fn recordings_recovered(count: usize) {
    let body = if count == 1 {
        "Quedó una reunión sin procesar de la última vez; se está procesando ahora.".to_string()
    } else {
        format!("Quedaron {count} reuniones sin procesar de la última vez; se están procesando ahora.")
    };
    notify("Recuperando grabaciones", &body);
}

pub fn config_invalid(err: &str) {
    notify(
        "La configuración tiene un error",
        &format!("Se sigue usando la anterior. {err}"),
    );
}

/// The calendar and the mic detector usually both see the same meeting a few
/// minutes apart (event starts, then you join the call). One nudge is enough.
const START_REMINDER_DEDUP: Duration = Duration::from_secs(10 * 60);
static LAST_START_REMINDER: Mutex<Option<Instant>> = Mutex::new(None);

fn start_reminder(summary: &str, body: &str) {
    let mut last = LAST_START_REMINDER.lock();
    if last.is_some_and(|t| t.elapsed() < START_REMINDER_DEDUP) {
        return;
    }
    *last = Some(Instant::now());
    notify(summary, body);
}

pub fn meeting_detected(app_name: &str) {
    start_reminder(
        "¿Reunión en curso?",
        &format!("{app_name} está usando el micrófono. {}", start_hint()),
    );
}

pub fn calendar_meeting_starting(title: &str) {
    start_reminder(
        "¿Grabo la reunión?",
        &format!("«{title}» empieza ahora. {}", start_hint()),
    );
}

pub fn meeting_ended_still_recording() {
    notify(
        "¿Terminó la reunión?",
        "La app de la reunión soltó el micrófono y stt-md sigue grabando. Click en STT → Detener.",
    );
}

pub fn recording_auto_stopped(hours: i64) {
    notify(
        "Grabación detenida automáticamente",
        &format!("Llevaba {hours} h grabando; se detuvo y se está procesando."),
    );
}

pub fn recording_stopped_for_silence(minutes: u64) {
    notify(
        "Grabación detenida por silencio",
        &format!("Pasaron {minutes} min sin voz, así que se detuvo sola y la nota ya se está procesando. Si la reunión sigue, vuelve a grabar."),
    );
}

pub fn system_audio_unavailable() {
    notify(
        "Grabando solo el micrófono",
        "No se pudo capturar el audio del sistema: revisa el permiso de Grabación de pantalla para stt-md en Ajustes del Sistema → Privacidad.",
    );
}

pub fn recording_silent(minutes: u64, auto_stop_in: Option<u64>) {
    let tail = match auto_stop_in {
        Some(more) => format!("Si sigue así se detiene sola en {more} min."),
        None => "Click en STT → Detener.".to_string(),
    };
    notify(
        "¿Terminó la reunión?",
        &format!("Llevas {minutes} min sin voz y stt-md sigue grabando. {tail}"),
    );
}

pub fn system_audio_lost() {
    notify(
        "Se perdió el audio del sistema",
        "No se pudo recuperar la captura: desde aquí solo queda tu micrófono. Detén y vuelve a grabar si necesitas la otra voz.",
    );
}

pub fn remote_voice_missing(minutes: u64) {
    notify(
        "No está llegando la otra voz",
        &format!(
            "Llevas {minutes} min hablando sin que suene nada por el parlante. Revisa el audio de la reunión: lo que digan los demás no se está grabando."
        ),
    );
}
