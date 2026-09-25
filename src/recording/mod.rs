pub mod mic;
pub mod system_audio;
pub mod wav_writer;

use anyhow::{anyhow, Result};
use chrono::Local;
use crossbeam_channel::{unbounded, Sender};
use serde::{Deserialize, Serialize};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::paths;
use mic::MicCapture;
use system_audio::{
    new_last_audio, LastAudio, SystemAudioCapture, SYSTEM_AUDIO_CHANNELS, SYSTEM_AUDIO_SAMPLE_RATE,
};

/// How long the system tap may deliver nothing but digital silence, while the
/// mic still hears someone, before we call it dead and restart it. A phone
/// call taking over the audio session killed a 31-minute meeting at 14:41 and
/// the other side's half was never recorded. Long enough that a real pause in
/// the call can't look like a failure.
pub const SYSTEM_AUDIO_STALL: Duration = Duration::from_secs(90);

/// Each restart costs a fraction of a second of audio, so a tap that keeps
/// dying is a problem to report, not to keep papering over.
const MAX_SYSTEM_RESTARTS: u32 = 5;
use wav_writer::{LastVoice, WavSink};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AudioSource {
    #[default]
    MicOnly,
    MicAndSystem,
}

/// Paths of the WAV file(s) produced by a recording session. `sys_path` is
/// present only when system audio was captured (mic+system mode).
#[derive(Debug, Clone)]
pub struct RecordingOutput {
    pub mic_path: PathBuf,
    pub sys_path: Option<PathBuf>,
    /// Times the system tap died and had to be revived mid-recording.
    pub sys_restarts: u32,
}

/// The system-audio tap and everything needed to revive it.
///
/// The spare sender lives here on purpose: dropping the tap has to drop it
/// too, or the WAV writer keeps waiting on a channel nobody will close and
/// `stop()` hangs joining it. Keeping them in one owner is what makes that
/// mistake impossible rather than merely documented.
struct SystemTap {
    capture: Option<SystemAudioCapture>,
    tx: Sender<Vec<f32>>,
    last_audio: LastAudio,
    restarts: u32,
}

pub struct RecordingSession {
    pub started_at: Instant,
    pub source: AudioSource,
    mic: MicCapture,
    system: Option<SystemTap>,
    mic_wav: WavSink,
    sys_wav: Option<WavSink>,
    last_voice: LastVoice,
}

impl RecordingSession {
    pub fn start(source: AudioSource) -> Result<Self> {
        match source {
            AudioSource::MicOnly => Self::start_mic_only(),
            AudioSource::MicAndSystem => Self::start_mic_and_system(),
        }
    }

    /// Tries mic + system audio first; if SCStream fails (no Screen Recording
    /// permission, macOS <13, or other SCK error) falls back to mic-only and
    /// returns `(session, false)`. On success returns `(session, true)`.
    pub fn start_with_fallback() -> Result<(Self, bool)> {
        match Self::start_mic_and_system() {
            Ok(s) => Ok((s, true)),
            Err(e) => {
                eprintln!("[stt-md] system audio capture failed ({e:?}); falling back to mic-only");
                Self::start_mic_only().map(|s| (s, false))
            }
        }
    }

    fn start_mic_only() -> Result<Self> {
        let (tx, rx) = unbounded::<Vec<f32>>();
        let mic = MicCapture::start(tx)?;
        let path = paths::recordings_dir().join(format!("{}.wav", timestamp_base()));
        let last_voice: LastVoice = Arc::new(Mutex::new(Instant::now()));
        let mic_wav = WavSink::spawn(rx, mic.sample_rate, mic.channels, path, last_voice.clone())?;
        Ok(Self {
            started_at: Instant::now(),
            source: AudioSource::MicOnly,
            mic,
            system: None,
            mic_wav,
            sys_wav: None,
            last_voice,
        })
    }

    /// Mic and system audio are written to two separate WAVs (`<ts>-mic.wav`
    /// and `<ts>-sys.wav`), each in its capture's native format. Keeping the
    /// tracks apart avoids clipping from additive mixing and lets the
    /// transcription step label who spoke (local user vs. remote side).
    fn start_mic_and_system() -> Result<Self> {
        let (mic_tx, mic_rx) = unbounded::<Vec<f32>>();
        let (sys_tx, sys_rx) = unbounded::<Vec<f32>>();

        let mic = MicCapture::start(mic_tx)?;
        let sys_last_audio = new_last_audio();
        let system = SystemTap {
            capture: Some(SystemAudioCapture::start(
                sys_tx.clone(),
                sys_last_audio.clone(),
            )?),
            tx: sys_tx,
            last_audio: sys_last_audio,
            restarts: 0,
        };

        let base = timestamp_base();
        let mic_path = paths::recordings_dir().join(format!("{base}-mic.wav"));
        let sys_path = paths::recordings_dir().join(format!("{base}-sys.wav"));

        let last_voice: LastVoice = Arc::new(Mutex::new(Instant::now()));
        let mic_wav = WavSink::spawn(
            mic_rx,
            mic.sample_rate,
            mic.channels,
            mic_path,
            last_voice.clone(),
        )?;
        let sys_wav = WavSink::spawn(
            sys_rx,
            SYSTEM_AUDIO_SAMPLE_RATE,
            SYSTEM_AUDIO_CHANNELS,
            sys_path,
            last_voice.clone(),
        )?;

        Ok(Self {
            started_at: Instant::now(),
            source: AudioSource::MicAndSystem,
            mic,
            system: Some(system),
            mic_wav,
            sys_wav: Some(sys_wav),
            last_voice,
        })
    }

    /// How long neither the mic nor the system audio has carried voice.
    pub fn silent_for(&self) -> Duration {
        self.last_voice.lock().elapsed()
    }

    /// Whether the system tap went dead while the meeting is still going.
    ///
    /// Both halves matter: digital silence on its own is what a call on hold
    /// looks like, and a quiet stretch on its own is just nobody talking. Only
    /// the pair — someone speaking into the mic while the tap hands us exact
    /// zeros — means the audio we are supposed to be recording is gone.
    pub fn system_audio_stalled(&self) -> bool {
        let Some(tap) = self.system.as_ref() else {
            return false;
        };
        tap.restarts < MAX_SYSTEM_RESTARTS
            && tap.last_audio.lock().elapsed() >= SYSTEM_AUDIO_STALL
            && self.silent_for() < SYSTEM_AUDIO_STALL
    }

    /// Tears the dead SCStream down and opens a new one onto the same channel,
    /// so the WAV writer keeps appending to the file it already has. The gap is
    /// whatever SCK takes to hand over, well under a second.
    pub fn restart_system_audio(&mut self) -> Result<u32> {
        let Some(tap) = self.system.as_mut() else {
            return Err(anyhow!("esta sesión no está capturando audio del sistema"));
        };
        tap.restarts += 1;
        // Drop first: two taps on the same display fight over the audio unit.
        tap.capture = None;
        *tap.last_audio.lock() = Instant::now();
        tap.capture = Some(SystemAudioCapture::start(
            tap.tx.clone(),
            tap.last_audio.clone(),
        )?);
        Ok(tap.restarts)
    }

    /// How many times the tap had to be revived. Surfaces in the note so a
    /// transcript stitched across a dropout is never read as a clean one.
    pub fn system_restarts(&self) -> u32 {
        self.system.as_ref().map_or(0, |t| t.restarts)
    }

    pub fn stop(self) -> Result<RecordingOutput> {
        let output = RecordingOutput {
            mic_path: self.mic_wav.path.clone(),
            sys_path: self.sys_wav.as_ref().map(|w| w.path.clone()),
            sys_restarts: self.system_restarts(),
        };

        // Drop the captures first so every sender closes; each WAV writer then
        // sees Disconnected, drains and finalizes its file. `SystemTap` owns
        // the spare sender the watchdog needs, so letting it go releases both.
        drop(self.mic);
        drop(self.system);

        self.mic_wav
            .handle
            .join()
            .map_err(|_| anyhow!("mic wav writer thread panicked"))??;
        if let Some(sys_wav) = self.sys_wav {
            join_or_log("sys wav writer", sys_wav.handle);
        }
        Ok(output)
    }
}

fn timestamp_base() -> String {
    Local::now().format("%Y-%m-%d-%H%M%S").to_string()
}

fn join_or_log(name: &str, handle: JoinHandle<Result<()>>) {
    match handle.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => eprintln!("[stt-md] {name} error: {e:?}"),
        Err(_) => eprintln!("[stt-md] {name} thread panicked"),
    }
}
