use anyhow::Result;
use hound::WavReader;
use std::path::Path;

/// Loads a WAV file as mono f32 samples in [-1.0, 1.0], returning the file's sample rate.
pub fn load_wav_mono_f32(path: &Path) -> Result<(Vec<f32>, u32)> {
    let mut reader = WavReader::open(path)?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    let sample_rate = spec.sample_rate;

    let samples: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| s.map(|s| (s as f32) / (i16::MAX as f32)))
            .collect::<Result<Vec<_>, _>>()?,
        (hound::SampleFormat::Int, 32) => reader
            .samples::<i32>()
            .map(|s| s.map(|s| (s as f32) / (i32::MAX as f32)))
            .collect::<Result<Vec<_>, _>>()?,
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()?,
        (fmt, bits) => anyhow::bail!("unsupported wav format: {fmt:?} {bits}-bit"),
    };

    let mono = if channels == 1 {
        samples
    } else {
        samples
            .chunks_exact(channels)
            .map(|frame| frame.iter().sum::<f32>() / (channels as f32))
            .collect()
    };

    Ok((mono, sample_rate))
}

/// The longest stretch of exact digital silence in a track: where it starts
/// and how long it runs, in seconds. `None` when nothing flatlines for a
/// minute or more.
///
/// A system tap that dies keeps handing the recorder buffers of exact zeros,
/// so the WAV ends up the right length and only the transcript reveals that
/// half the meeting is missing a voice. Live audio never holds exact zeros for
/// minutes: whatever is playing carries a noise floor. Measured as the longest
/// run rather than the tail because a tap that gets revived mid-meeting leaves
/// its gap in the middle, and because a stray sound at the end is enough to
/// hide a dropout that lasted twenty minutes.
pub fn dead_air(samples: &[f32], sample_rate: u32) -> Option<(f32, f32)> {
    let min_run = sample_rate as usize * 60;
    let (mut best_start, mut best_len) = (0usize, 0usize);
    let mut run_start = 0usize;
    for (i, s) in samples.iter().enumerate() {
        if *s != 0.0 {
            if i - run_start > best_len {
                best_len = i - run_start;
                best_start = run_start;
            }
            run_start = i + 1;
        }
    }
    if samples.len() - run_start > best_len {
        best_len = samples.len() - run_start;
        best_start = run_start;
    }
    let rate = sample_rate as f32;
    (best_len >= min_run).then(|| (best_start as f32 / rate, best_len as f32 / rate))
}

/// Linear interpolation resample to 16kHz. Adequate for the speech band that
/// Whisper consumes; a windowed-sinc resampler would only matter for music.
pub fn resample_to_16k(samples: &[f32], from_rate: u32) -> Vec<f32> {
    if from_rate == 16_000 {
        return samples.to_vec();
    }
    let ratio = 16_000.0 / from_rate as f32;
    let out_len = (samples.len() as f32 * ratio).ceil() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f32 / ratio;
        let idx = src_pos as usize;
        let frac = src_pos - idx as f32;
        if idx + 1 < samples.len() {
            out.push(samples[idx] * (1.0 - frac) + samples[idx + 1] * frac);
        } else if idx < samples.len() {
            out.push(samples[idx]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: usize, rate: u32) -> Vec<f32> {
        (0..secs * rate as usize).map(|i| ((i % 97) as f32 / 97.0) - 0.5).collect()
    }

    #[test]
    fn dead_air_finds_a_tap_that_died_mid_meeting() {
        // 60 s of call, 20 min of zeros, then a stray sound at the very end:
        // the shape of the meeting that lost half of the other voice.
        let mut samples = tone(60, 16_000);
        samples.extend(std::iter::repeat_n(0.0, 16_000 * 60 * 20));
        samples.extend(tone(1, 16_000));
        let (at, run) = dead_air(&samples, 16_000).expect("a 20-minute hole is dead air");
        assert!((at - 60.0).abs() < 1.0, "starts where the audio stopped: {at}");
        assert!((run - 1200.0).abs() < 1.0, "runs until audio returns: {run}");
    }

    #[test]
    fn dead_air_ignores_ordinary_pauses() {
        let mut samples = tone(60, 16_000);
        samples.extend(std::iter::repeat_n(0.0, 16_000 * 30));
        samples.extend(tone(60, 16_000));
        assert_eq!(dead_air(&samples, 16_000), None);
    }


    #[test]
    fn resample_identity_at_16k() {
        let s = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_to_16k(&s, 16_000), s);
    }

    #[test]
    fn resample_halves_48k_to_16k() {
        let s: Vec<f32> = (0..48_000).map(|i| (i as f32).sin()).collect();
        let out = resample_to_16k(&s, 48_000);
        let expected = 16_000;
        assert!((out.len() as i64 - expected).abs() <= 2, "len={}", out.len());
    }

    #[test]
    fn resample_preserves_constant_signal() {
        let s = vec![0.5f32; 44_100];
        let out = resample_to_16k(&s, 44_100);
        assert!(out.iter().all(|v| (v - 0.5).abs() < 1e-6));
    }
}
