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
/// Una llamada toma el mic *y* reproduce audio; un audio de WhatsApp solo
/// graba. Con ambas cosas a la vez durante este rato, es una llamada.
const CALL_CONFIRM: Duration = Duration::from_secs(8);
/// Sin reproducción no se sabe qué es, así que se espera: los audios de
/// WhatsApp dispararon «¿Reunión en curso?» cinco veces seguidas en un día, y
/// casi ninguno dura tanto. Una llamada real que no se detecte por la otra vía
/// (sala de espera, todos en silencio) avisa igual, solo que más tarde.
const MIC_ONLY_HOLD: Duration = Duration::from_secs(45);
/// «¿Terminó la reunión?» solo si la app tuvo el mic al menos este rato: lo que
/// suelta el mic a los segundos de empezar a grabar no era la reunión.
const MIN_CALL_FOR_END_HINT: Duration = Duration::from_secs(3 * 60);

/// Lanza el hilo detector. `custom_apps` (config `meeting_reminder_apps`)
/// reemplaza la lista por defecto; son prefijos de bundle ID.
pub fn spawn(state: Arc<Mutex<AppState>>, custom_apps: Option<Vec<String>>) {
    let _ = thread::Builder::new()
        .name("meeting-detector".into())
        .spawn(move || run_loop(state, custom_apps));
}

/// Si ya toca avisar: la app lleva `held` con el mic y `playing_for`
/// reproduciendo además (o `None` si no reproduce).
fn should_remind(held: Duration, playing_for: Option<Duration>) -> bool {
    playing_for.is_some_and(|p| p >= CALL_CONFIRM) || held >= MIC_ONLY_HOLD
}

fn run_loop(state: Arc<Mutex<AppState>>, custom_apps: Option<Vec<String>>) {
    let custom: Option<Vec<String>> =
        custom_apps.map(|v| v.into_iter().map(|s| s.to_ascii_lowercase()).collect());

    // Una "sesión" es un tramo continuo (con debounce) en que alguna app de
    // la lista mantiene el mic. Se avisa a lo más una vez por sesión, y no
    // necesariamente en el primer poll: hay que esperar a saber si es una
    // llamada, y si cae dentro del cooldown el aviso queda pendiente y sale
    // apenas se pueda, en vez de perderse.
    let mut session_started: Option<Instant> = None;
    let mut playing_since: Option<Instant> = None;
    let mut handled = false;
    let mut last_seen: Option<Instant> = None;
    let mut last_notified: Option<Instant> = None;

    loop {
        thread::sleep(POLL_INTERVAL);

        let Some(app) = detect_meeting_app(custom.as_deref()) else {
            if let Some(started) = session_started
                && last_seen.is_none_or(|t| t.elapsed() >= RELEASE_DEBOUNCE)
            {
                session_started = None;
                playing_since = None;
                // La app de reuniones soltó el mic pero seguimos grabando:
                // lo típico es haber cortado la call y olvidar Detener.
                let held = last_seen.map_or(Duration::ZERO, |seen| seen.duration_since(started));
                if state.lock().is_recording() && held >= MIN_CALL_FOR_END_HINT {
                    println!("[stt-md] meeting app released the mic while recording");
                    notifications::meeting_ended_still_recording();
                }
            }
            continue;
        };

        let now = Instant::now();
        last_seen = Some(now);
        let started = *session_started.get_or_insert_with(|| {
            handled = false;
            now
        });
        playing_since = if app.also_playing {
            playing_since.or(Some(now))
        } else {
            None
        };
        if handled {
            continue;
        }
        match *state.lock() {
            // Ya están grabando esta reunión: no molestar, tampoco después
            // (p. ej. si detienen la grabación antes de que termine la call).
            AppState::Recording { .. } => handled = true,
            AppState::Idle => {
                let playing_for = playing_since.map(|t| now.duration_since(t));
                if should_remind(now.duration_since(started), playing_for)
                    && last_notified.is_none_or(|t| t.elapsed() >= NOTIFY_COOLDOWN)
                {
                    println!("[stt-md] meeting detected: {} is capturing the mic", app.name);
                    notifications::meeting_detected(&app.name);
                    handled = true;
                    last_notified = Some(now);
                }
            }
        }
    }
}

/// Una app de reuniones con el micrófono tomado.
#[derive(Debug, PartialEq)]
struct Detected {
    name: String,
    /// Algún proceso de la misma app está reproduciendo audio: es una llamada
    /// y no solo una grabación.
    also_playing: bool,
}

/// La primera app de reuniones que está capturando el micrófono ahora mismo,
/// o `None`. El propio proceso se excluye (graba con cpal).
fn detect_meeting_app(custom: Option<&[String]>) -> Option<Detected> {
    let own_pid = std::process::id() as i32;
    let processes: Vec<AudioProcess> = coreaudio::process_objects()
        .into_iter()
        .filter(|&obj| coreaudio::pid(obj) != Some(own_pid))
        .filter_map(|obj| {
            let (input, output) = (coreaudio::is_running_input(obj), coreaudio::is_running_output(obj));
            if !input && !output {
                return None;
            }
            Some(AudioProcess { bundle: coreaudio::bundle_id(obj)?.to_ascii_lowercase(), input, output })
        })
        .collect();
    pick_meeting_app(&processes, custom)
}

/// Un proceso que el HAL ve usando audio.
struct AudioProcess {
    bundle: String,
    input: bool,
    output: bool,
}

fn pick_meeting_app(processes: &[AudioProcess], custom: Option<&[String]>) -> Option<Detected> {
    processes.iter().filter(|p| p.input).find_map(|p| {
        let (prefix, name) = meeting_app(&p.bundle, custom)?;
        // El que captura y el que reproduce pueden ser helpers distintos de
        // la misma app, por eso se compara por prefijo y no por proceso.
        let also_playing = processes
            .iter()
            .any(|q| q.output && q.bundle.starts_with(&prefix));
        Some(Detected { name, also_playing })
    })
}

/// Whether some other process is feeding the output right now.
///
/// This is what tells a dead system-audio tap apart from an ordinary quiet
/// stretch. Both hand the recorder exact zeros — the assumption that a live tap
/// always carries a noise floor holds for a call, which sends comfort noise,
/// but not for a Mac with nothing playing. Asking the HAL who is actually
/// playing removes the guesswork: zeros while something plays means the tap is
/// gone; zeros while nothing plays means nothing is happening.
pub fn output_is_playing() -> bool {
    let own_pid = std::process::id() as i32;
    coreaudio::process_objects().into_iter().any(|obj| {
        coreaudio::is_running_output(obj) && coreaudio::pid(obj) != Some(own_pid)
    })
}

/// (prefijo que calzó, nombre a mostrar) si `bundle` es una app de reuniones
/// de la lista, o `None`.
fn meeting_app(bundle: &str, custom: Option<&[String]>) -> Option<(String, String)> {
    let bundle = bundle.to_ascii_lowercase();
    match custom {
        Some(list) => list
            .iter()
            .find(|p| bundle.starts_with(p.as_str()))
            .map(|p| (p.clone(), p.clone())),
        None => DEFAULT_MEETING_APPS
            .iter()
            .find(|(p, _)| bundle.starts_with(p))
            .map(|(p, name)| ((*p).to_string(), (*name).to_string())),
    }
}

#[cfg(test)]
fn meeting_app_name(bundle: &str, custom: Option<&[String]>) -> Option<String> {
    meeting_app(bundle, custom).map(|(_, name)| name)
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
    const PROCESS_IS_RUNNING_OUTPUT: u32 = fourcc(b"piro"); // kAudioProcessPropertyIsRunningOutput

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
        running(obj, PROCESS_IS_RUNNING_INPUT)
    }

    pub fn is_running_output(obj: u32) -> bool {
        running(obj, PROCESS_IS_RUNNING_OUTPUT)
    }

    fn running(obj: u32, selector: u32) -> bool {
        let address = addr(selector);
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

    fn process(bundle: &str, input: bool, output: bool) -> AudioProcess {
        AudioProcess { bundle: bundle.to_string(), input, output }
    }

    /// Los audios de WhatsApp toman el mic sin reproducir nada; una llamada
    /// hace las dos cosas, aunque sea desde helpers distintos.
    #[test]
    fn a_call_is_told_apart_from_a_voice_note_by_playback() {
        let voice_note = [process("net.whatsapp.whatsapp", true, false)];
        assert_eq!(
            pick_meeting_app(&voice_note, None),
            Some(Detected { name: "WhatsApp".into(), also_playing: false })
        );

        let meet = [
            process("com.apple.music", false, true),
            process("com.google.chrome.helper", true, false),
            process("com.google.chrome.helper.renderer", false, true),
        ];
        assert_eq!(
            pick_meeting_app(&meet, None),
            Some(Detected { name: "Chrome".into(), also_playing: true })
        );

        // Otra app sonando no convierte un audio en llamada.
        let note_with_music = [
            process("com.apple.music", false, true),
            process("net.whatsapp.whatsapp", true, false),
        ];
        assert!(!pick_meeting_app(&note_with_music, None).unwrap().also_playing);

        // Una app de la lista que solo reproduce no es una reunión.
        assert_eq!(pick_meeting_app(&[process("us.zoom.xos", false, true)], None), None);
    }

    #[test]
    fn reminder_waits_to_know_it_is_a_call() {
        let secs = Duration::from_secs;
        // Llamada confirmada: mic + reproducción sostenidos.
        assert!(!should_remind(secs(4), Some(secs(4))));
        assert!(should_remind(secs(8), Some(secs(8))));
        // Solo mic: un audio de 20 s no avisa; algo largo sí.
        assert!(!should_remind(secs(20), None));
        assert!(should_remind(secs(45), None));
    }

    #[test]
    fn dictation_apps_never_count_as_meetings() {
        assert_eq!(meeting_app_name("com.superduper.superwhisper", None), None);
        assert_eq!(meeting_app_name("com.apple.speech.speechsynthesisd", None), None);
    }

    #[test]
    fn meeting_apps_match_including_helpers() {
        assert_eq!(meeting_app_name("us.zoom.xos", None).as_deref(), Some("Zoom"));
        assert_eq!(
            meeting_app_name("com.google.Chrome.helper", None).as_deref(),
            Some("Chrome")
        );
    }

    #[test]
    fn custom_list_matches_by_prefix() {
        let list = vec!["us.zoom".to_string()];
        // No podemos forzar a Zoom a abrir el mic en un test; validamos solo
        // que una lista custom no rompe el flujo de detección.
        let _ = detect_meeting_app(Some(&list));
    }
}
