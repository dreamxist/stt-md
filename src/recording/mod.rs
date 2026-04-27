pub mod mic;
pub mod wav_writer;

use anyhow::{anyhow, Result};
use crossbeam_channel::unbounded;
use std::path::PathBuf;
use std::time::Instant;

use mic::MicCapture;
use wav_writer::WavSink;

pub struct RecordingSession {
    pub started_at: Instant,
    mic: MicCapture,
    wav: WavSink,
}

impl RecordingSession {
    pub fn start() -> Result<Self> {
        let (tx, rx) = unbounded::<Vec<f32>>();
        let mic = MicCapture::start(tx)?;
        let wav = WavSink::spawn(rx, mic.sample_rate, mic.channels)?;
        Ok(Self {
            started_at: Instant::now(),
            mic,
            wav,
        })
    }

    pub fn stop(self) -> Result<PathBuf> {
        // Drop the mic stream first; this closes the cpal callback and
        // therefore drops the Sender, which lets the WavSink's recv loop exit.
        let path = self.wav.path.clone();
        drop(self.mic);
        self.wav
            .handle
            .join()
            .map_err(|_| anyhow!("wav writer thread panicked"))??;
        Ok(path)
    }
}
