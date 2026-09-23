use anyhow::Result;
use crossbeam_channel::Receiver;
use hound::{SampleFormat, WavSpec, WavWriter};
use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use parking_lot::Mutex;

/// Buffer RMS above which the capture counts as someone talking (~-45 dBFS).
/// Room noise after a meeting ends sits around -50 dBFS.
const VOICE_RMS: f32 = 0.0056;

/// When any track last carried voice. Shared by the mic and system writers so
/// silence means both sides went quiet.
pub type LastVoice = Arc<Mutex<Instant>>;

pub struct WavSink {
    pub path: PathBuf,
    pub handle: JoinHandle<Result<()>>,
}

impl WavSink {
    pub fn spawn(
        rx: Receiver<Vec<f32>>,
        sample_rate: u32,
        channels: u16,
        path: PathBuf,
        last_voice: LastVoice,
    ) -> Result<Self> {
        let path_clone = path.clone();

        let handle = thread::spawn(move || -> Result<()> {
            let spec = WavSpec {
                channels,
                sample_rate,
                bits_per_sample: 16,
                sample_format: SampleFormat::Int,
            };
            let file = File::create(&path_clone)?;
            let mut writer = WavWriter::new(BufWriter::new(file), spec)?;

            while let Ok(samples) = rx.recv() {
                if !samples.is_empty() {
                    let rms = (samples.iter().map(|s| s * s).sum::<f32>()
                        / samples.len() as f32)
                        .sqrt();
                    if rms > VOICE_RMS {
                        *last_voice.lock() = Instant::now();
                    }
                }
                for s in samples {
                    let clamped = (s * (i16::MAX as f32)).clamp(i16::MIN as f32, i16::MAX as f32);
                    writer.write_sample(clamped as i16)?;
                }
            }
            writer.finalize()?;
            Ok(())
        });

        Ok(Self { path, handle })
    }
}
