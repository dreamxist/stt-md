use stt_md::{
    activity::KeepAwake, api_key, app_state, calendar_reminder, config, control, history, jobs, logging,
    meeting_detector, notifications, paths, pipeline, recording, sounds, usage,
};

mod app {
    pub mod icon;
    pub mod menu;
}

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Local};
use crossbeam_channel::{unbounded, Receiver, Sender};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use parking_lot::{Mutex, RwLock};
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tray_icon::menu::{MenuEvent, MenuId};
use tray_icon::TrayIcon;

use app::menu::{pretty_hotkey, Action, AppMenu, RECENT_NOTES};
use app_state::AppState;
use config::{Config, OutputMode};
use jobs::{Job, JobEvent, JobStatus, SharedConfig};
use pipeline::{RecordingInput, Stage};
use recording::RecordingSession;

/// Hard cap on a single recording; beyond this the Stop was forgotten.
const MAX_RECORDING_HOURS: i64 = 4;
/// Quiet time after which a still-running recording is probably a forgotten
/// Stop (meeting over, laptop left open).
const SILENCE_REMINDER: Duration = Duration::from_secs(5 * 60);
/// How often the menubar title, tooltip and status line are redrawn.
const REDRAW: Duration = Duration::from_millis(900);
/// How long the event loop sleeps when nothing wakes it. Clicks, the shortcut,
/// control commands and job progress all wake it themselves, so the timer is
/// only there for the clock in the menubar and the periodic checks.
const TICK_BUSY: Duration = Duration::from_millis(500);
const TICK_IDLE: Duration = Duration::from_secs(2);
const CONFIG_CHECK: Duration = Duration::from_secs(2);
const SPEND_REFRESH: Duration = Duration::from_secs(60);

const USAGE: &str = "\
stt-md                 abre la app en la menubar
stt-md toggle          empieza o detiene la grabación en la app abierta
stt-md start | stop    lo mismo, sin alternar
stt-md status          estado de la app abierta, en JSON
stt-md quit            cierra la app abierta; lo que esté grabando queda en cola
stt-md --selftest [s]  graba s segundos (20) y procesa, sin menubar";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--selftest") => {
            let seconds = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20);
            match selftest(seconds) {
                Ok(()) => println!("[selftest] ✓ todo el camino funciona"),
                Err(e) => {
                    eprintln!("[selftest] ✗ falló: {e:#}");
                    std::process::exit(1);
                }
            }
        }
        Some("help" | "--help" | "-h") => println!("{USAGE}"),
        Some(word) => match control::Command::parse(word) {
            Some(command) => match control::send(&paths::socket_path(), command) {
                Ok(reply) => {
                    println!("{reply}");
                    if reply.starts_with("error") {
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("{e:#}");
                    std::process::exit(1);
                }
            },
            None => {
                eprintln!("comando desconocido: {word}\n\n{USAGE}");
                std::process::exit(2);
            }
        },
        None => {
            if logging::redirect_to_file(&paths::log_file()) {
                println!(
                    "\n=== stt-md {} arrancó {} ===",
                    env!("CARGO_PKG_VERSION"),
                    Local::now().format("%Y-%m-%d %H:%M:%S")
                );
            }
            if let Err(e) = run() {
                let msg = format!("{e:#}");
                eprintln!("[stt-md] fatal: {msg}");
                logging::drain();
                // Launched as .app there is no terminal: surface the error
                // visually or the app just silently never appears.
                show_fatal_alert(&msg);
                std::process::exit(1);
            }
        }
    }
}

/// Record, stop and process without touching the menubar.
///
/// The permissions this app needs are bound to the signature of the bundle, so
/// every rebuild asks for them again — and the only way to know they took hold
/// used to be starting a real meeting by hand and hoping. Running the same
/// binary from the same bundle exercises the whole path under the identity that
/// actually holds the grants.
fn selftest(seconds: u64) -> anyhow::Result<()> {
    let cfg = Config::load_or_init()?;
    cfg.validate()?;

    let (session, used_system) = RecordingSession::start_with_fallback()?;
    println!(
        "[selftest] grabando {seconds}s — audio del sistema: {}",
        if used_system { "sí" } else { "NO (falta el permiso de Grabación de pantalla)" }
    );
    let started_at = Local::now();
    thread::sleep(Duration::from_secs(seconds));

    let output = session.stop()?;
    println!("[selftest] detenido sin colgarse: {}", output.mic_path.display());
    if let Some(sys) = &output.sys_path {
        println!("[selftest] pista del sistema: {}", sys.display());
    }

    let input = RecordingInput {
        mic_path: output.mic_path,
        sys_path: output.sys_path,
        started_at,
        duration_min: Some(1),
        calendar_title: None,
    };
    let meeting = pipeline::process(&input, &cfg, &mut |stage| {
        println!("[selftest] {}", stage.label());
    })?;
    println!("[selftest] nota escrita: {}", meeting.note_path.display());
    Ok(())
}

/// Two copies of the app (the installed one and the build in `dist/`) both
/// show up in Spotlight under the same name, and opening both put two icons in
/// the menubar that each recorded on their own. The first instance holds an
/// exclusive lock on a file next to the config; a second one finds it taken
/// and quits before drawing anything. The OS drops the lock if the app dies.
fn claim_single_instance() -> anyhow::Result<Option<std::fs::File>> {
    let path = Config::config_path().with_file_name("stt-md.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

fn configure_gemini_key() {
    let Some(key) = api_key::ask_for_gemini_key() else {
        return;
    };
    let result = api_key::validate_gemini_key(&key).and_then(|()| api_key::save_gemini_key(&key));
    match result {
        Ok(()) => api_key::show_message(
            "stt-md",
            "Listo: la key funciona y quedó guardada en el Llavero. Las próximas reuniones se resumen con Gemini.",
        ),
        Err(e) => api_key::show_message("stt-md", &format!("No se guardó la key: {e:#}")),
    }
}

fn show_fatal_alert(msg: &str) {
    let script = format!(
        "display alert \"stt-md no pudo iniciar\" message \"{}\" as critical",
        msg.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let _ = Command::new("osascript").args(["-e", &script]).status();
}

fn open(target: &Path) {
    if let Err(e) = Command::new("open").arg(target).status() {
        eprintln!("[stt-md] could not open {}: {e}", target.display());
    }
}

/// Opens a note where it is meant to be read: in Obsidian when it lives in a
/// vault, with whatever handles Markdown otherwise.
fn open_note(path: &Path, cfg: &Config) {
    if cfg.output_mode == OutputMode::Obsidian && path.starts_with(&cfg.vault_root) {
        let url = format!("obsidian://open?path={}", percent_encode(&path.to_string_lossy()));
        if Command::new("open").arg(&url).status().is_ok_and(|s| s.success()) {
            return;
        }
    }
    open(path);
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

fn mmss(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
    } else {
        format!("{:02}:{:02}", secs / 60, secs % 60)
    }
}

fn duration_min(started: DateTime<Local>, ended: DateTime<Local>) -> i64 {
    (ended - started).num_minutes().max(1)
}

/// Why a recording is being stopped.
#[derive(Debug, Clone, Copy, PartialEq)]
enum StopReason {
    User,
    MaxHours,
    /// Nobody spoke for the configured time; the meeting ended at the last voice.
    Silence,
}

/// The recording in progress and the queue entry that will outlive it.
struct ActiveRecording {
    session: RecordingSession,
    job: Job,
    used_system: bool,
    silence_notified: bool,
    remote_missing_notified: bool,
}

/// The job the worker is on, as the menubar shows it.
struct InProgress {
    job: Job,
    label: String,
    stage: Stage,
}

struct App {
    cfg: SharedConfig,
    cfg_mtime: Option<SystemTime>,
    menu: AppMenu,
    tray: Option<TrayIcon>,
    /// Whether the menubar currently shows text next to the icon.
    title_shown: bool,

    state: Arc<Mutex<AppState>>,
    recording: Option<ActiveRecording>,
    current_event: calendar_reminder::CurrentEvent,

    queue_dir: std::path::PathBuf,
    job_tx: Sender<Job>,
    job_events: Receiver<JobEvent>,
    /// Jobs handed to the worker that have not finished yet.
    queued: usize,
    in_progress: Option<InProgress>,
    failed: Vec<Job>,

    /// Kept alive: dropping it unregisters the shortcut.
    _hotkey: Option<GlobalHotKeyManager>,
    hotkey_label: Option<String>,
    menu_rx: Receiver<MenuId>,
    hotkey_rx: Receiver<()>,
    control_rx: Receiver<control::Request>,
    /// Held while recording or processing; see [`KeepAwake`].
    keep_awake: Option<KeepAwake>,
    key_tx: Sender<()>,
    key_rx: Receiver<()>,

    last_redraw: Instant,
    last_config_check: Instant,
    last_spend_refresh: Instant,
}

fn run() -> anyhow::Result<()> {
    let cfg = Config::load_or_init()?;
    println!("[stt-md] config loaded from {}", Config::config_path().display());
    match cfg.output_mode {
        OutputMode::Obsidian => println!("[stt-md] mode: obsidian → {}", cfg.vault_root.display()),
        OutputMode::Simple => println!("[stt-md] mode: simple   → {}", cfg.output_dir.display()),
    }
    cfg.validate()?;
    // Held for the life of the process: the event loop below never returns.
    let Some(_instance_lock) = claim_single_instance()? else {
        println!("[stt-md] already running, exiting");
        return Ok(());
    };

    let mut event_loop = EventLoopBuilder::<()>::with_user_event().build();
    // Whatever happens off the main thread wakes the loop through this
    // instead of waiting for its timer: an idle menubar app is napped by
    // macOS, and a napped timer made a `start` take ten seconds to be heard.
    let proxy = event_loop.create_proxy();
    let wake = move || {
        let _ = proxy.send_event(());
    };
    // Accessory = sin ícono en el Dock ni en Cmd-Tab; solo vive en la menubar.
    // tao fuerza Regular por defecto, pisando el LSUIElement del Info.plist.
    event_loop.set_activation_policy(ActivationPolicy::Accessory);

    // The shortcut has to be registered from the main thread, with the event
    // loop already created.
    let (hotkey_manager, hotkey_label) = register_hotkey(&cfg.hotkey);
    notifications::set_start_hint(match &hotkey_label {
        Some(keys) => format!("Pulsa {keys} o usa el menú STT para grabar."),
        None => String::new(),
    });

    let state = Arc::new(Mutex::new(AppState::Idle));
    let current_event: calendar_reminder::CurrentEvent = Arc::new(Mutex::new(None));
    if cfg.meeting_reminder {
        meeting_detector::spawn(state.clone(), cfg.meeting_reminder_apps.clone());
        calendar_reminder::spawn(state.clone(), current_event.clone());
    }

    let (menu_tx, menu_rx) = unbounded();
    MenuEvent::set_event_handler(Some({
        let wake = wake.clone();
        move |event: MenuEvent| {
            let _ = menu_tx.send(event.id);
            wake();
        }
    }));
    let (hotkey_tx, hotkey_rx) = unbounded();
    GlobalHotKeyEvent::set_event_handler(Some({
        let wake = wake.clone();
        move |event: GlobalHotKeyEvent| {
            if event.state() == HotKeyState::Pressed {
                let _ = hotkey_tx.send(());
                wake();
            }
        }
    }));
    let (control_tx, control_rx) = unbounded();
    let served = control::serve(&paths::socket_path(), {
        let wake = wake.clone();
        move |request| {
            let _ = control_tx.send(request);
            wake();
        }
    });
    if let Err(e) = served {
        eprintln!("[stt-md] control socket unavailable: {e:#}");
    }

    let cfg_mtime = config_mtime();
    let hotkey_text = hotkey_label.is_some().then_some(cfg.hotkey.as_str());
    let menu = AppMenu::new(hotkey_text)?;
    let cfg: SharedConfig = Arc::new(RwLock::new(Arc::new(cfg)));

    let queue_dir = paths::queue_dir();
    let (events_tx, job_events) = unbounded();
    let job_tx = jobs::spawn_worker(queue_dir.clone(), cfg.clone(), move |event| {
        let _ = events_tx.send(event);
        wake();
    });
    // The key dialog blocks while open, so it runs off the main thread and
    // reports back here; menu items can only be touched from the event loop.
    let (key_tx, key_rx) = unbounded::<()>();

    let mut app = App {
        cfg,
        cfg_mtime,
        tray: Some(menu.build_tray()?),
        menu,
        title_shown: false,
        state,
        recording: None,
        current_event,
        queue_dir,
        job_tx,
        job_events,
        queued: 0,
        in_progress: None,
        failed: Vec::new(),
        _hotkey: hotkey_manager,
        hotkey_label,
        menu_rx,
        hotkey_rx,
        control_rx,
        keep_awake: None,
        key_tx,
        key_rx,
        last_redraw: Instant::now() - REDRAW,
        last_config_check: Instant::now(),
        last_spend_refresh: Instant::now(),
    };
    app.refresh_summary_items();
    app.refresh_recent();
    app.refresh_recordings_item();
    app.resume_queue();

    event_loop.run(move |event, _, control_flow| {
        // Once per turn of the loop, after whatever woke it was delivered.
        if !matches!(event, Event::MainEventsCleared) {
            return;
        }
        if app.tick() {
            *control_flow = ControlFlow::Exit;
            return;
        }
        let sleep = if app.is_busy() { TICK_BUSY } else { TICK_IDLE };
        *control_flow = ControlFlow::WaitUntil(Instant::now() + sleep);
    });
}

fn register_hotkey(spec: &str) -> (Option<GlobalHotKeyManager>, Option<String>) {
    let spec = spec.trim();
    if spec.is_empty() {
        return (None, None);
    }
    let registered = spec
        .parse::<HotKey>()
        .map_err(|e| e.to_string())
        .and_then(|hotkey| {
            let manager = GlobalHotKeyManager::new().map_err(|e| e.to_string())?;
            manager.register(hotkey).map_err(|e| e.to_string())?;
            Ok(manager)
        });
    match registered {
        Ok(manager) => {
            let label = pretty_hotkey(spec);
            println!("[stt-md] global shortcut {label} registered");
            (Some(manager), Some(label))
        }
        Err(e) => {
            eprintln!("[stt-md] could not register the shortcut «{spec}»: {e}");
            (None, None)
        }
    }
}

fn config_mtime() -> Option<SystemTime> {
    std::fs::metadata(Config::config_path()).and_then(|m| m.modified()).ok()
}

impl App {
    fn config(&self) -> Arc<Config> {
        self.cfg.read().clone()
    }

    /// One pass of the event loop. Returns `true` to quit.
    fn tick(&mut self) -> bool {
        // First, so a quit from the menu or the socket sees a job the worker
        // has just finished as finished.
        while let Ok(event) = self.job_events.try_recv() {
            self.handle_job_event(event);
        }
        while let Ok(id) = self.menu_rx.try_recv() {
            if let Some(action) = self.menu.action_for(&id)
                && self.handle(action)
            {
                return true;
            }
        }
        while self.hotkey_rx.try_recv().is_ok() {
            self.toggle();
        }
        while let Ok(request) = self.control_rx.try_recv() {
            if request.command == control::Command::Quit {
                // Answer first: `quit` ends by waiting for the log, which
                // also gives the reply time to reach the client.
                let _ = request.reply.send("cerrando".to_string());
                self.quit();
                return true;
            }
            let reply = self.handle_command(request.command);
            let _ = request.reply.send(reply);
        }
        while self.key_rx.try_recv().is_ok() {
            self.refresh_summary_items();
        }

        self.watch_recording();

        if self.last_config_check.elapsed() >= CONFIG_CHECK {
            self.last_config_check = Instant::now();
            self.reload_config_if_changed();
        }
        // `resummarize` and `--selftest` spend from other processes too, so
        // the menu rereads the ledger now and then.
        if self.last_spend_refresh.elapsed() >= SPEND_REFRESH {
            self.last_spend_refresh = Instant::now();
            self.refresh_spend_item();
        }
        if self.last_redraw.elapsed() >= REDRAW {
            self.redraw();
        }
        self.keep_awake = match (self.is_busy(), self.keep_awake.take()) {
            (true, held) => held.or_else(|| Some(KeepAwake::begin("grabando o procesando una reunión"))),
            (false, _) => None,
        };
        false
    }

    /// Recording, or with meetings still to turn into notes.
    fn is_busy(&self) -> bool {
        self.recording.is_some() || self.queued > 0
    }

    /// Returns `true` when the action is to quit.
    fn handle(&mut self, action: Action) -> bool {
        let cfg = self.config();
        match action {
            Action::Toggle => self.toggle(),
            Action::OpenNote(path) => open_note(&path, &cfg),
            Action::Retry(job) => self.retry(job),
            Action::ShowNotesFolder => open(cfg.notes_root()),
            Action::EditConfig => {
                // `-t`: the default text editor, whatever claims .toml.
                let _ = Command::new("open").arg("-t").arg(Config::config_path()).status();
            }
            Action::OpenLog => open(&paths::log_file()),
            Action::ShowRecordings => open(&paths::recordings_dir()),
            Action::ConfigureKey => {
                let tx = self.key_tx.clone();
                thread::spawn(move || {
                    configure_gemini_key();
                    let _ = tx.send(());
                });
            }
            Action::RemoveKey => {
                match api_key::delete_gemini_key() {
                    Ok(()) => api_key::show_message(
                        "stt-md",
                        "Listo: la API key se borró del Llavero. Las reuniones se van a resumir con el modelo local.",
                    ),
                    Err(e) => api_key::show_message("stt-md", &format!("No se pudo borrar: {e:#}")),
                }
                self.refresh_summary_items();
            }
            Action::Quit => {
                self.quit();
                return true;
            }
        }
        false
    }

    fn handle_command(&mut self, command: control::Command) -> String {
        use control::Command::*;
        let recording = self.recording.is_some();
        match command {
            Start if recording => "ya estaba grabando".to_string(),
            Stop if !recording => "no hay grabación en curso".to_string(),
            Start | Stop | Toggle => {
                self.toggle();
                match (recording, self.recording.is_some()) {
                    (false, true) => "grabando".to_string(),
                    (true, false) => "detenida; procesando la nota".to_string(),
                    (false, false) => "error: no se pudo iniciar la grabación".to_string(),
                    (true, true) => "error: no se pudo detener la grabación".to_string(),
                }
            }
            Status => self.status_json(),
            // Handled by the caller: it has to stop the event loop.
            Quit => String::new(),
        }
    }

    fn status_json(&self) -> String {
        serde_json::json!({
            "recording": self.recording.is_some(),
            "elapsed_secs": self.recording.as_ref().map(|r| r.session.started_at.elapsed().as_secs()),
            "system_audio": self.recording.as_ref().map(|r| r.used_system),
            "queued": self.queued,
            "processing": self.in_progress.as_ref().map(|p| {
                serde_json::json!({ "meeting": p.label, "stage": p.stage.label() })
            }),
            "failed": self.failed.len(),
        })
        .to_string()
    }

    fn toggle(&mut self) {
        if self.recording.is_some() {
            self.stop_recording(StopReason::User);
        } else {
            self.start_recording();
        }
    }

    fn start_recording(&mut self) {
        let (session, used_system) = match RecordingSession::start_with_fallback() {
            Ok(started) => started,
            Err(e) => {
                eprintln!("[stt-md] failed to start recording: {e:?}");
                notifications::recording_failed(&format!("{e:#}"));
                return;
            }
        };
        sounds::play_start();
        let calendar_title = self.current_event.lock().clone();
        println!(
            "[stt-md] recording started (system audio: {}{})",
            if used_system { "yes" } else { "no — fallback to mic-only" },
            calendar_title
                .as_deref()
                .map(|t| format!("; calendar: {t}"))
                .unwrap_or_default()
        );
        if !used_system {
            notifications::system_audio_unavailable();
        }

        // On record from the first second: if the app dies mid-meeting, this
        // file is how the next launch knows there is audio to rescue.
        let (mic_path, sys_path) = session.paths();
        let job = Job::for_recording(Local::now(), mic_path, sys_path, calendar_title);
        if let Err(e) = job.save(&self.queue_dir) {
            eprintln!("[stt-md] could not record job {}: {e:#}", job.id);
        }

        *self.state.lock() = AppState::Recording { started_at: session.started_at };
        self.recording = Some(ActiveRecording {
            session,
            job,
            used_system,
            silence_notified: false,
            remote_missing_notified: false,
        });
        self.redraw();
    }

    fn stop_recording(&mut self, reason: StopReason) {
        let Some(active) = self.recording.take() else {
            return;
        };
        sounds::play_stop();
        *self.state.lock() = AppState::Idle;

        // Wall clock, not Instant: Instant stops while the Mac sleeps,
        // undercounting long meetings. Stopped for silence, the meeting ended
        // when the last voice did, not when the app noticed.
        let ended = match reason {
            StopReason::Silence => {
                Local::now() - chrono::Duration::from_std(active.session.silent_for()).unwrap_or_default()
            }
            _ => Local::now(),
        };
        let mut job = active.job;
        job.duration_min = Some(duration_min(job.started_at, ended));
        job.status = JobStatus::Pending;

        match active.session.stop() {
            Ok(output) => println!(
                "[stt-md] recording stopped ({reason:?}, {} min, {} tap restarts)",
                job.duration_min.unwrap_or(0),
                output.sys_restarts
            ),
            // Whatever reached the disk before the writer failed is still a
            // recording: let the queue try it instead of dropping the meeting.
            Err(e) => eprintln!("[stt-md] stop error, processing what was written: {e:?}"),
        }
        self.enqueue(job);
        self.redraw();
    }

    fn enqueue(&mut self, job: Job) {
        if let Err(e) = job.save(&self.queue_dir) {
            eprintln!("[stt-md] could not save job {}: {e:#}", job.id);
        }
        if self.job_tx.send(job).is_ok() {
            self.queued += 1;
        }
    }

    fn retry(&mut self, mut job: Job) {
        // A retry asked for by hand starts the crash count over.
        job.attempts = 0;
        self.failed.retain(|j| j.id != job.id);
        self.refresh_failed();
        println!("[stt-md] job {}: retry requested", job.id);
        self.enqueue(job);
        self.redraw();
    }

    /// Picks up what a previous run left in the queue.
    fn resume_queue(&mut self) {
        let recovered = jobs::recover(&self.queue_dir);
        if !recovered.run.is_empty() {
            println!("[stt-md] resuming {} unfinished job(s)", recovered.run.len());
            notifications::recordings_recovered(recovered.run.len());
        }
        for job in recovered.run {
            self.enqueue(job);
        }
        self.failed = recovered.failed;
        self.refresh_failed();
        if let Some(days) = self.config().keep_recordings_days {
            jobs::sweep_old_recordings(&paths::recordings_dir(), &self.queue_dir, days, SystemTime::now());
            self.refresh_recordings_item();
        }
    }

    fn handle_job_event(&mut self, event: JobEvent) {
        match event {
            JobEvent::Started { job } => {
                self.in_progress = Some(InProgress { label: job.label(), job, stage: Stage::Loading });
            }
            JobEvent::Stage { stage, .. } => {
                if let Some(p) = self.in_progress.as_mut() {
                    p.stage = stage;
                }
            }
            JobEvent::Done { meeting, .. } => {
                self.job_finished();
                notifications::meeting_saved(&meeting.title, meeting.duration_min, meeting.model.as_deref());
                if let Some(err) = &meeting.daily_error {
                    notifications::daily_not_updated(err);
                }
                self.refresh_recent();
                self.refresh_spend_item();
            }
            JobEvent::Skipped { reason, .. } => {
                self.job_finished();
                notifications::meeting_skipped(&reason);
            }
            JobEvent::Failed { job, error } => {
                self.job_finished();
                notifications::meeting_failed(&job.label(), &error);
                self.failed.retain(|j| j.id != job.id);
                self.failed.push(job);
                self.refresh_failed();
            }
        }
        self.redraw();
    }

    fn job_finished(&mut self) {
        self.queued = self.queued.saturating_sub(1);
        self.in_progress = None;
        self.refresh_recordings_item();
    }

    /// Everything that can go wrong, or go on too long, while recording.
    fn watch_recording(&mut self) {
        let auto_stop = Duration::from_secs(self.config().auto_stop_silence_min * 60);
        let Some(active) = self.recording.as_mut() else {
            return;
        };

        // A dead system tap is invisible from the outside: the timer keeps
        // running and the WAV keeps growing, only full of zeros. Revive it
        // while the meeting is still happening — afterwards there is nothing
        // to recover. Quietly: most of the time the tap was fine and the
        // other side was simply not talking.
        if active.session.system_audio_stalled() {
            match active.session.restart_system_audio() {
                Ok(n) => println!("[stt-md] system audio silent while something plays; restarted tap ({n})"),
                Err(e) => {
                    eprintln!("[stt-md] could not restart system audio: {e:?}");
                    notifications::system_audio_lost();
                }
            }
        }

        // Nothing is playing and the user keeps talking: the other side is not
        // reaching the output at all, so there is nothing to restart and the
        // only thing that helps is saying so before the meeting ends.
        let missing = active.session.system_audio_missing();
        if missing && !active.remote_missing_notified {
            let mins = recording::SYSTEM_AUDIO_MISSING.as_secs() / 60;
            println!("[stt-md] nothing on the output for {mins} min while the mic hears voice");
            notifications::remote_voice_missing(mins);
        }
        active.remote_missing_notified = missing;

        // Nudge once per quiet stretch; talking again re-arms the reminder.
        let silent_for = active.session.silent_for();
        let quiet = silent_for >= SILENCE_REMINDER;
        if quiet && !active.silence_notified {
            let mins = SILENCE_REMINDER.as_secs() / 60;
            println!("[stt-md] no voice for {mins} min while recording");
            let until_auto_stop = (auto_stop > SILENCE_REMINDER)
                .then(|| (auto_stop - SILENCE_REMINDER).as_secs() / 60);
            notifications::recording_silent(mins, until_auto_stop);
        }
        active.silence_notified = quiet;

        let reason = if !auto_stop.is_zero() && silent_for >= auto_stop {
            Some(StopReason::Silence)
        } else if Local::now() - active.job.started_at >= chrono::Duration::hours(MAX_RECORDING_HOURS) {
            // Safety net for a forgotten Stop: a recording that ran for two
            // days produced a 12 GB WAV that never became a note.
            Some(StopReason::MaxHours)
        } else {
            None
        };
        match reason {
            Some(StopReason::Silence) => {
                let mins = auto_stop.as_secs() / 60;
                println!("[stt-md] no voice for {mins} min; stopping automatically");
                notifications::recording_stopped_for_silence(mins);
            }
            Some(StopReason::MaxHours) => {
                println!("[stt-md] recording hit {MAX_RECORDING_HOURS}h; stopping automatically");
                notifications::recording_auto_stopped(MAX_RECORDING_HOURS);
            }
            _ => {}
        }
        if let Some(reason) = reason {
            self.stop_recording(reason);
        }
    }

    /// An edit to the config file applies to the next meeting without a
    /// relaunch. A file that doesn't parse leaves the previous one in force.
    fn reload_config_if_changed(&mut self) {
        let mtime = config_mtime();
        if mtime == self.cfg_mtime {
            return;
        }
        self.cfg_mtime = mtime;
        match Config::load_or_init().and_then(|cfg| cfg.validate().map(|()| cfg)) {
            Ok(cfg) => {
                println!("[stt-md] config reloaded");
                *self.cfg.write() = Arc::new(cfg);
                self.refresh_summary_items();
            }
            Err(e) => {
                eprintln!("[stt-md] config not reloaded: {e:#}");
                notifications::config_invalid(&format!("{e:#}"));
            }
        }
    }

    fn quit(&mut self) {
        // A recording in progress is closed and left in the queue: the next
        // launch turns it into its note.
        if let Some(active) = self.recording.take() {
            let mut job = active.job;
            job.duration_min = Some(duration_min(job.started_at, Local::now()));
            job.status = JobStatus::Pending;
            if let Err(e) = active.session.stop() {
                eprintln!("[stt-md] stop error on quit: {e:?}");
            }
            let _ = job.save(&self.queue_dir);
            println!("[stt-md] quitting mid-recording; job {} stays queued", job.id);
        }
        // The job being processed counted an attempt when it began, which is
        // how a meeting that crashes the app every time is caught. Quitting
        // is not a crash: give the attempt back, or three quits during a
        // long summary park a healthy meeting as failed.
        // Only while its file is still there: a job the worker just finished
        // must not come back to life and write its note a second time.
        if let Some(InProgress { job, .. }) = self.in_progress.take()
            && self.queue_dir.join(format!("{}.json", job.id)).exists()
        {
            let mut job = job;
            job.attempts = job.attempts.saturating_sub(1);
            job.status = JobStatus::Pending;
            let _ = job.save(&self.queue_dir);
        }
        if self.queued > 0 {
            println!(
                "[stt-md] quitting with {} job(s) unfinished; they resume on next launch",
                self.queued
            );
        }
        let _ = std::fs::remove_file(paths::socket_path());
        self.tray.take();
        logging::drain();
    }

    // --- what the menubar shows ---------------------------------------------

    fn processing_line(&self) -> Option<String> {
        if self.queued == 0 {
            return None;
        }
        let waiting = match self.queued.saturating_sub(1) {
            0 => String::new(),
            n => format!(" (+{n} en cola)"),
        };
        Some(match &self.in_progress {
            Some(p) => format!("Procesando reunión del {}: {}{waiting}", p.label, p.stage.label()),
            None => format!("Procesando{waiting}…"),
        })
    }

    fn redraw(&mut self) {
        self.last_redraw = Instant::now();
        let processing = self.processing_line();

        let (title, status) = match &self.recording {
            Some(active) => {
                let timer = mmss(active.session.started_at.elapsed().as_secs());
                self.menu.toggle.set_text(format!("Detener — {timer}"));
                let source = if active.used_system { "mic + sistema" } else { "solo mic" };
                let mut status = format!("● Grabando {timer} ({source})");
                if let Some(p) = &processing {
                    status.push_str(&format!(" · {}", p.to_lowercase()));
                }
                (Some(format!(" {timer}")), status)
            }
            None => {
                self.menu.toggle.set_text("Empezar reunión");
                let title = processing.as_ref().map(|_| match &self.in_progress {
                    Some(InProgress { stage: Stage::Transcribing { percent }, .. }) => {
                        format!(" {percent}%")
                    }
                    _ => " …".to_string(),
                });
                let status = processing.unwrap_or_else(|| match &self.hotkey_label {
                    Some(keys) => format!("Listo para grabar · {keys}"),
                    None => "Listo para grabar".to_string(),
                });
                (title, status)
            }
        };
        self.menu.status.set_text(&status);

        match title {
            Some(title) => {
                if let Some(tray) = self.tray.as_ref() {
                    tray.set_title(Some(title));
                    let _ = tray.set_tooltip(Some(status));
                }
                self.title_shown = true;
            }
            // tray-icon 0.19 on macOS has no working way to clear a title
            // once set; only recreating the NSStatusItem does.
            None if self.title_shown => {
                self.title_shown = false;
                self.tray.take();
                match self.menu.build_tray() {
                    Ok(tray) => self.tray = Some(tray),
                    Err(e) => eprintln!("[stt-md] failed to rebuild tray: {e:?}"),
                }
            }
            None => {}
        }
    }

    fn refresh_summary_items(&self) {
        let cfg = self.config();
        let has_key = api_key::gemini_key().is_some();
        let chain = cfg.summary_models();
        let uses_gemini = chain.first().is_some_and(|m| m.starts_with("gemini-"));
        self.menu.summarizer.set_text(match (uses_gemini, has_key) {
            (true, true) => format!("Resumen: {} (respaldo local)", chain[0]),
            (true, false) => format!("Resumen: local, falta la API key de {}", chain[0]),
            (false, _) => format!("Resumen: local ({})", cfg.ollama_model),
        });
        self.menu.key.set_text(if has_key {
            "Cambiar API key de Gemini…"
        } else {
            "Configurar API key de Gemini…"
        });
        self.menu.remove_key.set_enabled(has_key);
        self.refresh_spend_item();
    }

    fn refresh_spend_item(&self) {
        let today = Local::now().date_naive();
        let t = usage::month_total(today);
        self.menu.spend.set_text(format!(
            "Gasto Gemini {}: US${} · {} resumen{}",
            usage::month_short_es(today),
            format!("{:.2}", t.cost_usd).replace('.', ","),
            t.calls,
            if t.calls == 1 { "" } else { "es" }
        ));
    }

    fn refresh_recent(&mut self) {
        self.menu.set_recent(&history::recent(RECENT_NOTES));
    }

    fn refresh_failed(&mut self) {
        if self.menu.set_failed(&self.failed)
            && let Some(tray) = self.tray.as_ref()
        {
            self.menu.apply(tray);
        }
    }

    fn refresh_recordings_item(&self) {
        let gb = dir_size(&paths::recordings_dir()) as f64 / 1e9;
        self.menu.recordings.set_text(format!(
            "Mostrar grabaciones ({} GB)",
            format!("{gb:.1}").replace('.', ",")
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn note_paths_survive_the_trip_through_an_obsidian_url() {
        assert_eq!(
            percent_encode("/Users/x/vault/notas/semana-46/reunión de año.md"),
            "%2FUsers%2Fx%2Fvault%2Fnotas%2Fsemana-46%2Freuni%C3%B3n%20de%20a%C3%B1o.md"
        );
    }

    #[test]
    fn timers_grow_an_hour_field_only_when_needed() {
        assert_eq!(mmss(0), "00:00");
        assert_eq!(mmss(754), "12:34");
        assert_eq!(mmss(3_725), "1:02:05");
    }

    #[test]
    fn a_meeting_lasts_at_least_a_minute() {
        let start = Local.with_ymd_and_hms(2026, 11, 10, 17, 0, 0).unwrap();
        assert_eq!(duration_min(start, start + chrono::Duration::seconds(20)), 1);
        assert_eq!(duration_min(start, start + chrono::Duration::minutes(63)), 63);
        // A clock that jumped back must not produce a negative duration.
        assert_eq!(duration_min(start, start - chrono::Duration::minutes(5)), 1);
    }
}
