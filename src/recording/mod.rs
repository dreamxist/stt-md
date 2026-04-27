pub mod mic;
pub mod mixer;
pub mod system_audio;
pub mod wav_writer;

use anyhow::{anyhow, Result};
use crossbeam_channel::unbounded;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::Instant;

use mic::MicCapture;
use mixer::{spawn_mixer, MixerHandle};
use system_audio::{SystemAudioCapture, SYSTEM_AUDIO_CHANNELS, SYSTEM_AUDIO_SAMPLE_RATE};
use wav_writer::WavSink;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AudioSource {
    #[default]
    MicOnly,
    MicAndSystem,
}

pub struct RecordingSession {
    pub started_at: Instant,
    pub source: AudioSource,
    mic: MicCapture,
    system: Option<SystemAudioCapture>,
    mixer: Option<MixerHandle>,
    wav: WavSink,
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
        let wav = WavSink::spawn(rx, mic.sample_rate, mic.channels)?;
        Ok(Self {
            started_at: Instant::now(),
            source: AudioSource::MicOnly,
            mic,
            system: None,
            mixer: None,
            wav,
        })
    }

    fn start_mic_and_system() -> Result<Self> {
        let (mic_tx, mic_rx) = unbounded::<Vec<f32>>();
        let (sys_tx, sys_rx) = unbounded::<Vec<f32>>();
        let (mixed_tx, mixed_rx) = unbounded::<Vec<f32>>();

        let mic = MicCapture::start(mic_tx)?;
        let system = SystemAudioCapture::start(sys_tx)?;

        let mixer = spawn_mixer(
            mic_rx,
            sys_rx,
            mic.sample_rate,
            mic.channels,
            SYSTEM_AUDIO_SAMPLE_RATE,
            mixed_tx,
        );

        // Mixed stream is mono @ 48kHz regardless of mic native config.
        let wav = WavSink::spawn(mixed_rx, SYSTEM_AUDIO_SAMPLE_RATE, SYSTEM_AUDIO_CHANNELS)?;

        Ok(Self {
            started_at: Instant::now(),
            source: AudioSource::MicAndSystem,
            mic,
            system: Some(system),
            mixer: Some(mixer),
            wav,
        })
    }

    pub fn stop(self) -> Result<PathBuf> {
        let path = self.wav.path.clone();

        // Drop mic and system streams first so their senders close. The mixer
        // (if present) will detect Disconnected, drain remaining buffers, drop
        // its own out Sender, which lets the WAV writer finalize.
        drop(self.mic);
        drop(self.system);

        if let Some(mixer) = self.mixer {
            join_or_log("mixer", mixer.handle);
        }

        self.wav
            .handle
            .join()
            .map_err(|_| anyhow!("wav writer thread panicked"))??;
        Ok(path)
    }
}

fn join_or_log(name: &str, handle: JoinHandle<Result<()>>) {
    match handle.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => eprintln!("[stt-md] {name} error: {e:?}"),
        Err(_) => eprintln!("[stt-md] {name} thread panicked"),
    }
}
