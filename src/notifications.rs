use std::path::Path;

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

pub fn meeting_detected(app_name: &str) {
    let _ = notify_rust::Notification::new()
        .summary("¿Reunión en curso?")
        .body(&format!(
            "{app_name} está usando el micrófono. Click en STT en la menubar → Empezar reunión."
        ))
        .appname("stt-md")
        .show();
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
