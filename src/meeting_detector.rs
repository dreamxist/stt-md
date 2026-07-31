//! "¿Reunión en curso?" — avisa cuando una app de reuniones empieza a usar el
//! micrófono y stt-md está idle, para no olvidar grabar.
//!
//! Usa los process objects de CoreAudio (macOS 14+) para saber *qué* proceso
//! está capturando input. La detección es por lista blanca de bundle IDs:
//! apps de dictado (superwhisper y similares) nunca disparan el aviso. En
//! macOS < 14 la propiedad no existe y el detector queda inactivo en silencio.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::app_state::AppState;
use crate::notifications;

/// (prefijo de bundle ID en minúsculas, nombre mostrado en la notificación).
/// Los navegadores cubren Meet / Teams web / Discord web; el proceso que
/// captura puede ser un helper (p. ej. `com.google.chrome.helper`), por eso
/// se compara por prefijo.
const DEFAULT_MEETING_APPS: &[(&str, &str)] = &[
    ("us.zoom.xos", "Zoom"),
    ("com.microsoft.teams", "Teams"),
    ("com.cisco.webex", "Webex"),
    ("com.webex", "Webex"),
    ("com.google.chrome", "Chrome"),
    ("net.imput.helium", "Helium"),
    ("com.apple.webkit", "Safari"), // Safari captura vía el proceso WebKit GPU
    ("com.apple.safari", "Safari"),
    ("org.mozilla.firefox", "Firefox"),
    ("com.microsoft.edgemac", "Edge"),
    ("com.brave.browser", "Brave"),
    ("company.thebrowser.browser", "Arc"),
    ("com.vivaldi.vivaldi", "Vivaldi"),
    ("com.tinyspeck.slackmacgap", "Slack"),
    ("com.hnc.discord", "Discord"),
    ("com.apple.facetime", "FaceTime"),
    ("net.whatsapp.whatsapp", "WhatsApp"),
];

const POLL_INTERVAL: Duration = Duration::from_secs(4);
/// Un corte de mic más corto que esto (cambio de dispositivo, reconexión de
/// Meet) no cuenta como fin de reunión: sin esto, un parpadeo del mic
/// reinicia la sesión y puede re-avisar por la misma reunión.
const RELEASE_DEBOUNCE: Duration = Duration::from_secs(30);
/// Resguardo extra entre avisos de sesiones distintas.
const NOTIFY_COOLDOWN: Duration = Duration::from_secs(60);

/// Lanza el hilo detector. `custom_apps` (config `meeting_reminder_apps`)
/// reemplaza la lista por defecto; son prefijos de bundle ID.
pub fn spawn(state: Arc<Mutex<AppState>>, custom_apps: Option<Vec<String>>) {
    let _ = thread::Builder::new()
        .name("meeting-detector".into())
        .spawn(move || run_loop(state, custom_apps));
}

fn run_loop(state: Arc<Mutex<AppState>>, custom_apps: Option<Vec<String>>) {
    let custom: Option<Vec<String>> =
        custom_apps.map(|v| v.into_iter().map(|s| s.to_ascii_lowercase()).collect());

    // Una "sesión" es un tramo continuo (con debounce) en que alguna app de
    // la lista mantiene el mic. Se avisa a lo más una vez por sesión, y no
    // necesariamente en el primer poll: si la reunión parte durante
    // Processing (post-proceso de la reunión anterior) o dentro del cooldown,
    // el aviso queda pendiente y sale apenas se pueda, en vez de perderse.
    let mut in_session = false;
    let mut handled = false;
    let mut last_seen: Option<Instant> = None;
    let mut last_notified: Option<Instant> = None;

    loop {
        thread::sleep(POLL_INTERVAL);

        let Some(app) = detect_meeting_app(custom.as_deref()) else {
            if in_session && last_seen.is_none_or(|t| t.elapsed() >= RELEASE_DEBOUNCE) {
                in_session = false;
            }
            continue;
        };

        last_seen = Some(Instant::now());
        if !in_session {
            in_session = true;
            handled = false;
        }
        if handled {
            continue;
        }
        match *state.lock() {
            // Ya están grabando esta reunión: no molestar, tampoco después
            // (p. ej. si detienen la grabación antes de que termine la call).
            AppState::Recording { .. } => handled = true,
            // Esperar a Idle: reunión nueva durante el post-proceso.
            AppState::Processing => {}
            AppState::Idle => {
                if last_notified.is_none_or(|t| t.elapsed() >= NOTIFY_COOLDOWN) {
                    println!("[stt-md] meeting detected: {app} is capturing the mic");
                    notifications::meeting_detected(&app);
                    handled = true;
                    last_notified = Some(Instant::now());
                }
            }
        }
    }
}

/// Nombre de la primera app de reuniones que está capturando el micrófono
/// ahora mismo, o `None`. El propio proceso se excluye (graba con cpal).
fn detect_meeting_app(custom: Option<&[String]>) -> Option<String> {
    let own_pid = std::process::id() as i32;
    for obj in coreaudio::process_objects() {
        if !coreaudio::is_running_input(obj) {
            continue;
        }
        if coreaudio::pid(obj) == Some(own_pid) {
            continue;
        }
        let Some(bundle) = coreaudio::bundle_id(obj) else {
            continue;
        };
        let bundle = bundle.to_ascii_lowercase();
        match custom {
            Some(list) => {
                if let Some(p) = list.iter().find(|p| bundle.starts_with(p.as_str())) {
                    return Some(p.clone());
                }
            }
            None => {
                if let Some((_, name)) =
                    DEFAULT_MEETING_APPS.iter().find(|(p, _)| bundle.starts_with(p))
                {
                    return Some((*name).to_string());
                }
            }
        }
    }
    None
}

/// FFI mínima a CoreAudio para los process objects (AudioHardware.h, macOS 14+).
/// Sin permisos TCC: solo lee metadatos del HAL, nunca toca el audio.
mod coreaudio {
    use std::ffi::{c_char, c_void, CStr};

    #[repr(C)]
    struct PropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    const SYSTEM_OBJECT: u32 = 1; // kAudioObjectSystemObject
    const SCOPE_GLOBAL: u32 = fourcc(b"glob");
    const ELEMENT_MAIN: u32 = 0;
    const PROCESS_OBJECT_LIST: u32 = fourcc(b"prs#"); // kAudioHardwarePropertyProcessObjectList
    const PROCESS_PID: u32 = fourcc(b"ppid"); // kAudioProcessPropertyPID
    const PROCESS_BUNDLE_ID: u32 = fourcc(b"pbid"); // kAudioProcessPropertyBundleID
    const PROCESS_IS_RUNNING_INPUT: u32 = fourcc(b"piri"); // kAudioProcessPropertyIsRunningInput

    const fn fourcc(b: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*b)
    }

    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        fn AudioObjectGetPropertyDataSize(
            object: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            out_size: *mut u32,
        ) -> i32;
        fn AudioObjectGetPropertyData(
            object: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            io_size: *mut u32,
            out_data: *mut c_void,
        ) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringGetCString(s: *const c_void, buf: *mut c_char, size: isize, encoding: u32) -> u8;
        fn CFRelease(cf: *const c_void);
    }
    const CF_STRING_UTF8: u32 = 0x0800_0100;

    fn addr(selector: u32) -> PropertyAddress {
        PropertyAddress { selector, scope: SCOPE_GLOBAL, element: ELEMENT_MAIN }
    }

    /// AudioObjectIDs de todos los procesos que el HAL conoce. Vec vacío en
    /// macOS < 14 (la propiedad no existe) o ante cualquier error.
    pub fn process_objects() -> Vec<u32> {
        let address = addr(PROCESS_OBJECT_LIST);
        let mut size: u32 = 0;
        let status = unsafe {
            AudioObjectGetPropertyDataSize(SYSTEM_OBJECT, &address, 0, std::ptr::null(), &mut size)
        };
        if status != 0 || size == 0 {
            return Vec::new();
        }
        let mut ids = vec![0u32; size as usize / size_of::<u32>()];
        let status = unsafe {
            AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                ids.as_mut_ptr().cast(),
            )
        };
        if status != 0 {
            return Vec::new();
        }
        ids.truncate(size as usize / size_of::<u32>());
        ids
    }

    pub fn is_running_input(obj: u32) -> bool {
        let address = addr(PROCESS_IS_RUNNING_INPUT);
        let mut value: u32 = 0;
        let mut size = size_of::<u32>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                obj,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                (&mut value as *mut u32).cast(),
            )
        };
        status == 0 && value != 0
    }

    pub fn pid(obj: u32) -> Option<i32> {
        let address = addr(PROCESS_PID);
        let mut value: i32 = 0;
        let mut size = size_of::<i32>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                obj,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                (&mut value as *mut i32).cast(),
            )
        };
        (status == 0).then_some(value)
    }

    pub fn bundle_id(obj: u32) -> Option<String> {
        let address = addr(PROCESS_BUNDLE_ID);
        let mut cf: *const c_void = std::ptr::null();
        let mut size = size_of::<*const c_void>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                obj,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                (&mut cf as *mut *const c_void).cast(),
            )
        };
        if status != 0 || cf.is_null() {
            return None;
        }
        let mut buf = [0 as c_char; 256];
        let ok =
            unsafe { CFStringGetCString(cf, buf.as_mut_ptr(), buf.len() as isize, CF_STRING_UTF8) };
        unsafe { CFRelease(cf) };
        if ok == 0 {
            return None;
        }
        let s = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned();
        (!s.is_empty()).then_some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Valida la FFI en la máquina de CI/dev: los FourCC correctos no truenan
    /// y las propiedades se leen sin panic. Con `-- --nocapture` imprime qué
    /// apps están capturando el mic ahora, útil para probar la lista.
    #[test]
    fn coreaudio_process_enumeration_does_not_crash() {
        let objs = coreaudio::process_objects();
        println!("process objects: {}", objs.len());
        for &obj in &objs {
            if coreaudio::is_running_input(obj) {
                println!(
                    "  capturing mic: pid={:?} bundle={:?}",
                    coreaudio::pid(obj),
                    coreaudio::bundle_id(obj)
                );
            }
        }
        let detected = detect_meeting_app(None);
        println!("meeting app detected: {detected:?}");
    }

    #[test]
    fn custom_list_matches_by_prefix() {
        let list = vec!["us.zoom".to_string()];
        // No podemos forzar a Zoom a abrir el mic en un test; validamos solo
        // que una lista custom no rompe el flujo de detección.
        let _ = detect_meeting_app(Some(&list));
    }
}
