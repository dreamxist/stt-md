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

pub fn meeting_failed(err: &str) {
    let _ = notify_rust::Notification::new()
        .summary("Error procesando reunión")
        .body(err)
        .appname("stt-md")
        .show();
}
