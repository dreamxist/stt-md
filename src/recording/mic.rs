use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use crossbeam_channel::Sender;

pub struct MicCapture {
    _stream: Stream,
    pub sample_rate: u32,
    pub channels: u16,
}

// cpal::Stream is !Send on macOS but we keep it pinned to the thread that
// constructed it (the controller thread). Same trick as stt-tomi.
unsafe impl Send for MicCapture {}

impl MicCapture {
    pub fn start(audio_tx: Sender<Vec<f32>>) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .context("no default input device available")?;
        let supported = device
            .default_input_config()
            .context("device has no default input config")?;
        let sample_rate = supported.sample_rate().0;
        let channels = supported.channels();
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();

        let err_fn = |err| eprintln!("[mic] stream error: {err}");

        let stream = match format {
            SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    let _ = audio_tx.send(data.to_vec());
                },
                err_fn,
                None,
            )?,
            SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    let v = data
                        .iter()
                        .map(|s| (*s as f32) / (i16::MAX as f32))
                        .collect();
                    let _ = audio_tx.send(v);
                },
                err_fn,
                None,
            )?,
            SampleFormat::U16 => device.build_input_stream(
                &config,
                move |data: &[u16], _| {
                    let v = data
                        .iter()
                        .map(|s| ((*s as f32) - 32768.0) / 32768.0)
                        .collect();
                    let _ = audio_tx.send(v);
                },
                err_fn,
                None,
            )?,
            other => anyhow::bail!("unsupported sample format: {other:?}"),
        };

        stream.play()?;

        Ok(Self {
            _stream: stream,
            sample_rate,
            channels,
        })
    }
}
