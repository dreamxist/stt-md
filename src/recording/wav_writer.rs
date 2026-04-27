use anyhow::Result;
use chrono::Local;
use crossbeam_channel::Receiver;
use hound::{SampleFormat, WavSpec, WavWriter};
use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;
use std::thread::{self, JoinHandle};

use crate::paths;

pub struct WavSink {
    pub path: PathBuf,
    pub handle: JoinHandle<Result<()>>,
}

impl WavSink {
    pub fn spawn(rx: Receiver<Vec<f32>>, sample_rate: u32, channels: u16) -> Result<Self> {
        let timestamp = Local::now().format("%Y-%m-%d-%H%M%S").to_string();
        let path = paths::recordings_dir().join(format!("{timestamp}.wav"));
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
