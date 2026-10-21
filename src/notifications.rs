//! Todas las notificaciones de la app.
//!
//! Notify-rust en macOS (UNUserNotificationCenter) llamado desde un hilo
//! worker rompe el menú del tray en macOS 26: el ícono se veía pero el
//! click no desplegaba nada. Por eso `show_guarded` no muestra la
//! notificación donde se llama: la encola por un canal y el event loop
//! principal la drena con `drain()` (solo existe show en el hilo principal).
//! Sin receiver registrado (CLI helpers), cae a mostrar inline.

use std::path::Path;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use parking_lot::Mutex;

pub struct Spec {
    summary: String,
    body: String,
}

static TX: Mutex<Option<Sender<Spec>>> = Mutex::new(None);

/// Crea el canal de notificaciones y devuelve el receiver para que el event
/// loop principal lo drene con `drain()`. Llamar una sola vez, al arrancar.
pub fn main_thread_receiver() -> Receiver<Spec> {
    let (tx, rx) = unbounded();
    *TX.lock() = Some(tx);
    rx
}

/// Muestra todas las notificaciones pendientes. SOLO desde el hilo principal.
pub fn drain(rx: &Receiver<Spec>) {
    while let Ok(spec) = rx.try_recv() {
        let _ = notify_rust::Notification::new()
            .summary(&spec.summary)
            .body(&spec.body)
            .appname("stt-md")
            .show();
    }
}

fn show_guarded(summary: &str, body: &str) {
    let queued = TX
        .lock()
        .clone()
        .is_some_and(|tx| tx.send(Spec { summary: summary.into(), body: body.into() }).is_ok());
    if !queued {
        // Sin receiver (CLI): inline, que es el comportamiento viejo.
        let _ = notify_rust::Notification::new()
            .summary(summary)
            .body(body)
            .appname("stt-md")
            .show();
    }
}

pub fn meeting_saved(meeting_path: &Path) {
    let body = meeting_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| meeting_path.display().to_string());
    show_guarded("Reunión guardada", &body);
}

pub fn recording_failed(err: &str) {
    show_guarded("No se pudo iniciar la grabación", err);
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
    show_guarded(summary, body);
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
    show_guarded("Error procesando reunión", err);
}

pub fn meeting_ended_still_recording() {
    show_guarded(
        "¿Terminó la reunión?",
        "La app de la reunión soltó el micrófono y stt-md sigue grabando. Click en STT → Detener.",
    );
}

pub fn recording_auto_stopped(hours: i64) {
    show_guarded(
        "Grabación detenida automáticamente",
        &format!("Llevaba {hours} h grabando; se detuvo y se está procesando."),
    );
}

pub fn system_audio_unavailable() {
    show_guarded(
        "Grabando solo el micrófono",
        "No se pudo capturar el audio del sistema: revisa el permiso de Grabación de pantalla para stt-md en Ajustes del Sistema → Privacidad.",
    );
}

pub fn recording_silent(minutes: u64) {
    show_guarded(
        "¿Terminó la reunión?",
        &format!(
            "Llevas {minutes} min sin voz y stt-md sigue grabando. Click en STT → Detener."
        ),
    );
}

pub fn system_audio_restarted() {
    show_guarded(
        "Se cayó el audio del sistema",
        "La otra voz dejó de llegar y stt-md reinició la captura. Revisa que la reunión siga sonando por el parlante.",
    );
}

pub fn system_audio_lost() {
    show_guarded(
        "Se perdió el audio del sistema",
        "No se pudo recuperar la captura: desde aquí solo queda tu micrófono. Detén y vuelve a grabar si necesitas la otra voz.",
    );
}

pub fn remote_voice_missing(minutes: u64) {
    show_guarded(
        "No está llegando la otra voz",
        &format!(
            "Llevas {minutes} min hablando sin que suene nada por el parlante. Revisa el audio de la reunión: lo que digan los demás no se está grabando."
        ),
    );
}
