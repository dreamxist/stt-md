pub mod mic;
pub mod system_audio;
pub mod wav_writer;

use anyhow::{anyhow, Result};
use chrono::Local;
use crossbeam_channel::unbounded;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::Instant;

use crate::paths;
use mic::MicCapture;
use system_audio::{SystemAudioCapture, SYSTEM_AUDIO_CHANNELS, SYSTEM_AUDIO_SAMPLE_RATE};
use wav_writer::WavSink;

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
}

pub struct RecordingSession {
    pub started_at: Instant,
    pub source: AudioSource,
    mic: MicCapture,
    system: Option<SystemAudioCapture>,
    mic_wav: WavSink,
    sys_wav: Option<WavSink>,
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
        let mic_wav = WavSink::spawn(rx, mic.sample_rate, mic.channels, path)?;
        Ok(Self {
            started_at: Instant::now(),
            source: AudioSource::MicOnly,
            mic,
            system: None,
            mic_wav,
            sys_wav: None,
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
        let system = SystemAudioCapture::start(sys_tx)?;

        let base = timestamp_base();
        let mic_path = paths::recordings_dir().join(format!("{base}-mic.wav"));
        let sys_path = paths::recordings_dir().join(format!("{base}-sys.wav"));

        let mic_wav = WavSink::spawn(mic_rx, mic.sample_rate, mic.channels, mic_path)?;
        let sys_wav = WavSink::spawn(
            sys_rx,
            SYSTEM_AUDIO_SAMPLE_RATE,
            SYSTEM_AUDIO_CHANNELS,
            sys_path,
        )?;

        Ok(Self {
            started_at: Instant::now(),
            source: AudioSource::MicAndSystem,
            mic,
            system: Some(system),
            mic_wav,
            sys_wav: Some(sys_wav),
        })
    }

    pub fn stop(self) -> Result<RecordingOutput> {
        let output = RecordingOutput {
            mic_path: self.mic_wav.path.clone(),
            sys_path: self.sys_wav.as_ref().map(|w| w.path.clone()),
        };

        // Drop mic and system streams first so their senders close; each WAV
        // writer then sees Disconnected, drains, and finalizes its file.
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
