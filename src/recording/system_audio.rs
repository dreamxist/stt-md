use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use screencapturekit::prelude::*;
use screencapturekit::stream::configuration::audio::{AudioChannelCount, AudioSampleRate};

pub const SYSTEM_AUDIO_SAMPLE_RATE: u32 = 48_000;
pub const SYSTEM_AUDIO_CHANNELS: u16 = 1;

pub struct SystemAudioCapture {
    stream: SCStream,
}

unsafe impl Send for SystemAudioCapture {}

struct AudioHandler {
    tx: Sender<Vec<f32>>,
}

impl SCStreamOutputTrait for AudioHandler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
        if of_type != SCStreamOutputType::Audio {
            return;
        }
        let Some(buffer_list) = sample.audio_buffer_list() else {
            return;
        };
        let mut samples: Vec<f32> = Vec::new();
        for buf in buffer_list.iter() {
            let bytes = buf.data();
            // SCK with channel_count=1 returns PCM Float32 mono. Decode without
            // assuming pointer alignment: each f32 is 4 little-endian bytes.
            for chunk in bytes.chunks_exact(4) {
                samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
        }
        if !samples.is_empty() {
            let _ = self.tx.send(samples);
        }
    }
}

impl SystemAudioCapture {
    pub fn start(audio_tx: Sender<Vec<f32>>) -> Result<Self> {
        let content = SCShareableContent::get().context(
            "no se pudo enumerar contenido (permiso de Screen Recording denegado en System Settings → Privacy)",
        )?;
        let display = content
            .displays()
            .into_iter()
            .next()
            .context("no hay display disponible para SCK")?;

        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
            .build();

        // Video tiene que estar configurado aunque solo nos interesa audio.
        // Pedimos 2x2 @ 1fps para minimizar el costo de capturar frames que ignoramos.
        let frame_interval = CMTime::new(1, 1);
        let config = SCStreamConfiguration::new()
            .with_width(2)
            .with_height(2)
            .with_minimum_frame_interval(&frame_interval)
            .with_captures_audio(true)
            .with_sample_rate(AudioSampleRate::Rate48000)
            .with_channel_count(AudioChannelCount::Mono)
            .with_excludes_current_process_audio(true);

        let mut stream = SCStream::new(&filter, &config);
        stream.add_output_handler(AudioHandler { tx: audio_tx }, SCStreamOutputType::Audio);
        stream
            .start_capture()
            .context("no se pudo iniciar SCStream (permiso denegado o macOS <13)")?;

        Ok(Self { stream })
    }
}

impl Drop for SystemAudioCapture {
    fn drop(&mut self) {
        let _ = self.stream.stop_capture();
    }
}
