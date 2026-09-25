use std::path::Path;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

pub fn meeting_saved(meeting_path: &Path) {
    let body = meeting_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| meeting_path.display().to_string());
    let _ = notify_rust::Notification::new()
        .summary("Reunión guardada")
        .body(&body)
        .appname("stt-md")
        .show();
}

pub fn recording_failed(err: &str) {
    let _ = notify_rust::Notification::new()
        .summary("No se pudo iniciar la grabación")
        .body(err)
        .appname("stt-md")
        .show();
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
    let _ = notify_rust::Notification::new()
        .summary(summary)
        .body(body)
        .appname("stt-md")
        .show();
}

pub fn meeting_detected(app_name: &str) {
    start_reminder(
        "¿Reunión en curso?",
        &format!("{app_name} está usando el micrófono. Click en STT en la menubar → Empezar reunión."),
    );
}

pub fn calendar_meeting_starting(title: &str) {
    start_reminder(
        "¿Grabo la reunión?",
        &format!("«{title}» empieza ahora. Click en STT en la menubar → Empezar reunión."),
    );
}

pub fn meeting_failed(err: &str) {
    let _ = notify_rust::Notification::new()
        .summary("Error procesando reunión")
        .body(err)
        .appname("stt-md")
        .show();
}

pub fn meeting_ended_still_recording() {
    let _ = notify_rust::Notification::new()
        .summary("¿Terminó la reunión?")
        .body("La app de la reunión soltó el micrófono y stt-md sigue grabando. Click en STT → Detener.")
        .appname("stt-md")
        .show();
}

pub fn recording_auto_stopped(hours: i64) {
    let _ = notify_rust::Notification::new()
        .summary("Grabación detenida automáticamente")
        .body(&format!("Llevaba {hours} h grabando; se detuvo y se está procesando."))
        .appname("stt-md")
        .show();
}

pub fn system_audio_unavailable() {
    let _ = notify_rust::Notification::new()
        .summary("Grabando solo el micrófono")
        .body("No se pudo capturar el audio del sistema: revisa el permiso de Grabación de pantalla para stt-md en Ajustes del Sistema → Privacidad.")
        .appname("stt-md")
        .show();
}

pub fn recording_silent(minutes: u64) {
    let _ = notify_rust::Notification::new()
        .summary("¿Terminó la reunión?")
        .body(&format!(
            "Llevas {minutes} min sin voz y stt-md sigue grabando. Click en STT → Detener."
        ))
        .appname("stt-md")
        .show();
}

pub fn system_audio_restarted() {
    let _ = notify_rust::Notification::new()
        .summary("Se cayó el audio del sistema")
        .body("La otra voz dejó de llegar y stt-md reinició la captura. Revisa que la reunión siga sonando por el parlante.")
        .appname("stt-md")
        .show();
}

pub fn system_audio_lost() {
    let _ = notify_rust::Notification::new()
        .summary("Se perdió el audio del sistema")
        .body("No se pudo recuperar la captura: desde aquí solo queda tu micrófono. Detén y vuelve a grabar si necesitas la otra voz.")
        .appname("stt-md")
        .show();
}

pub fn remote_voice_missing(minutes: u64) {
    let _ = notify_rust::Notification::new()
        .summary("No está llegando la otra voz")
        .body(&format!(
            "Llevas {minutes} min hablando sin que suene nada por el parlante. Revisa el audio de la reunión: lo que digan los demás no se está grabando."
        ))
        .appname("stt-md")
        .show();
}
