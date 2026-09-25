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

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use std::time::Duration;

    /// The writer stops at `Disconnected`, which only happens once *every*
    /// sender is gone — including spares held elsewhere. The watchdog keeps one
    /// so a revived tap writes into the same file, and forgetting to drop it
    /// before `stop()` joins the thread hangs the whole app on Detener.
    #[test]
    fn writer_finishes_only_once_every_sender_is_gone() {
        let dir = std::env::temp_dir().join(format!("stt-md-wavsink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = unbounded::<Vec<f32>>();
        let spare = tx.clone();
        let last_voice: LastVoice = Arc::new(Mutex::new(Instant::now()));
        let sink = WavSink::spawn(rx, 16_000, 1, dir.join("t.wav"), last_voice).unwrap();

        tx.send(vec![0.0; 16]).unwrap();
        drop(tx);
        thread::sleep(Duration::from_millis(100));
        assert!(
            !sink.handle.is_finished(),
            "writer finished while a spare sender was still alive"
        );

        drop(spare);
        for _ in 0..100 {
            if sink.handle.is_finished() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            sink.handle.is_finished(),
            "writer never finished after the last sender dropped"
        );
        sink.handle.join().unwrap().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
