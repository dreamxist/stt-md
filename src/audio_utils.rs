use anyhow::Result;
use hound::WavReader;
use std::path::Path;

/// Whisper's input rate.
pub const TARGET_RATE: u32 = 16_000;

/// Loads a WAV file as mono f32 samples in [-1.0, 1.0], returning the file's sample rate.
pub fn load_wav_mono_f32(path: &Path) -> Result<(Vec<f32>, u32)> {
    let mut reader = WavReader::open(path)?;
    let sample_rate = reader.spec().sample_rate;
    let mut mono = Vec::new();
    for_each_mono_chunk(&mut reader, |chunk| mono.extend_from_slice(chunk))?;
    Ok((mono, sample_rate))
}

/// Loads a WAV straight to 16 kHz mono, resampling as it reads.
///
/// A two-hour 48 kHz track is 1.4 GB as f32 before it is resampled; decoding
/// it in chunks keeps only the 16 kHz result in memory.
pub fn load_wav_16k_mono(path: &Path) -> Result<Vec<f32>> {
    let mut reader = WavReader::open(path)?;
    let spec = reader.spec();
    let frames = reader.duration() as usize;
    let mut resampler = Resampler::new(spec.sample_rate);
    let mut out = Vec::with_capacity(
        (frames as u64 * TARGET_RATE as u64 / spec.sample_rate.max(1) as u64) as usize + 16,
    );
    for_each_mono_chunk(&mut reader, |chunk| resampler.push(chunk, &mut out))?;
    resampler.finish(&mut out);
    Ok(out)
}

const CHUNK_FRAMES: usize = 1 << 16;

/// Decodes the WAV in chunks, downmixed to mono f32, without ever holding the
/// whole file.
fn for_each_mono_chunk<R: std::io::Read>(
    reader: &mut WavReader<R>,
    mut sink: impl FnMut(&[f32]),
) -> Result<()> {
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let mut frame = Vec::with_capacity(channels);
    let mut chunk = Vec::with_capacity(CHUNK_FRAMES);
    let mut push = |sample: f32, chunk: &mut Vec<f32>, sink: &mut dyn FnMut(&[f32])| {
        frame.push(sample);
        if frame.len() == channels {
            chunk.push(frame.iter().sum::<f32>() / channels as f32);
            frame.clear();
            if chunk.len() == CHUNK_FRAMES {
                sink(chunk);
                chunk.clear();
            }
        }
    };
    match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => {
            for s in reader.samples::<i16>() {
                push(s? as f32 / i16::MAX as f32, &mut chunk, &mut sink);
            }
        }
        (hound::SampleFormat::Int, 32) => {
            for s in reader.samples::<i32>() {
                push(s? as f32 / i32::MAX as f32, &mut chunk, &mut sink);
            }
        }
        (hound::SampleFormat::Float, 32) => {
            for s in reader.samples::<f32>() {
                push(s?, &mut chunk, &mut sink);
            }
        }
        (fmt, bits) => anyhow::bail!("unsupported wav format: {fmt:?} {bits}-bit"),
    }
    if !chunk.is_empty() {
        sink(&chunk);
    }
    Ok(())
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
///
/// A short flatline at the very end is the call hanging up before the
/// recording was stopped, not a dead tap: it is ignored, otherwise the note
/// warns that the summary is partial when nothing was lost.
pub fn dead_air(samples: &[f32], sample_rate: u32) -> Option<(f32, f32)> {
    let min_run = sample_rate as usize * 60;
    let min_trailing_run = sample_rate as usize * 180;
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
    let trailing = samples.len() - run_start;
    if trailing >= min_trailing_run && trailing > best_len {
        best_len = trailing;
        best_start = run_start;
    }
    let rate = sample_rate as f32;
    (best_len >= min_run).then(|| (best_start as f32 / rate, best_len as f32 / rate))
}

/// Resamples to 16 kHz in one go. See [`Resampler`].
pub fn resample_to_16k(samples: &[f32], from_rate: u32) -> Vec<f32> {
    if from_rate == TARGET_RATE {
        return samples.to_vec();
    }
    let mut resampler = Resampler::new(from_rate);
    let mut out = Vec::with_capacity(
        (samples.len() as u64 * TARGET_RATE as u64 / from_rate.max(1) as u64) as usize + 16,
    );
    resampler.push(samples, &mut out);
    resampler.finish(&mut out);
    out
}

/// Zero crossings of the sinc kept on each side of the filter's centre.
const SINC_ZEROS: f64 = 12.0;
/// Where the low-pass starts rolling off, as a share of the narrower Nyquist.
const CUTOFF_MARGIN: f64 = 0.92;
/// Above this many filter phases the table stops being worth it. Every rate a
/// sound card actually offers reduces to far fewer (44.1 kHz → 160).
const MAX_PHASES: usize = 4_096;

/// Streaming resampler to 16 kHz: a windowed-sinc low-pass evaluated at the
/// exact position of each output sample.
///
/// Positions are integers (`i * from / 16000` as quotient and remainder), not
/// floats, on purpose. The first version computed them in `f32`, which runs
/// out of mantissa after 2^24 samples: six minutes into a 48 kHz recording the
/// read position started snapping to every 2nd source sample, then every 4th,
/// 8th and 16th. A clean tone measured 153 dB of signal-to-noise in the first
/// minutes, 21 dB at minute 6, 7 dB at minute 24 and −0.5 dB — more noise than
/// signal — from minute 47 on, so the second half of every long meeting
/// reached Whisper as mush. The low-pass is what keeps everything above
/// 8 kHz from folding back into the speech band, which plain decimation did.
pub struct Resampler {
    /// `from / 16000` reduced: output `i` sits at source position `i * step / phases`.
    step: usize,
    phases: usize,
    /// `phases` rows of `2 * half + 1` taps; row `r` is the filter centred
    /// `r / phases` of a sample past an integer position.
    taps: Vec<f32>,
    half: usize,
    /// Source samples not yet fully consumed; `buf[0]` is sample `buf_start`.
    buf: Vec<f32>,
    buf_start: usize,
    /// Source samples seen so far.
    seen: usize,
    /// Next output index to produce.
    next_out: usize,
    passthrough: bool,
}

impl Resampler {
    pub fn new(from_rate: u32) -> Self {
        let from = from_rate.max(1) as usize;
        let to = TARGET_RATE as usize;
        let g = gcd(from, to);
        let (step, phases) = (from / g, to / g);
        let passthrough = from == to;

        // Cutoff in cycles per source sample: the narrower of the two Nyquists.
        let cutoff = 0.5 * (to as f64 / from as f64).min(1.0) * CUTOFF_MARGIN;
        let half = (SINC_ZEROS / (2.0 * cutoff)).ceil() as usize;
        let width = 2 * half + 1;
        let table_phases = if phases <= MAX_PHASES { phases } else { 0 };
        let mut taps = vec![0f32; table_phases * width];
        for r in 0..table_phases {
            let row = &mut taps[r * width..(r + 1) * width];
            let frac = r as f64 / phases as f64;
            let mut sum = 0f64;
            let mut values = vec![0f64; width];
            for (k, v) in values.iter_mut().enumerate() {
                // Distance from this tap's source sample to the output position.
                let x = k as f64 - half as f64 - frac;
                *v = sinc(2.0 * cutoff * x) * hann(x / (half as f64 + 1.0));
                sum += *v;
            }
            for (t, v) in row.iter_mut().zip(values) {
                *t = (v / sum) as f32;
            }
        }
        Self {
            step,
            phases,
            taps,
            half,
            buf: Vec::new(),
            buf_start: 0,
            seen: 0,
            next_out: 0,
            passthrough,
        }
    }

    /// Feeds source samples; appends whatever output they complete.
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<f32>) {
        if self.passthrough {
            out.extend_from_slice(samples);
            return;
        }
        self.buf.extend_from_slice(samples);
        self.seen += samples.len();
        self.drain(out, false);
    }

    /// Emits the tail that was waiting for samples that will never come.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        if !self.passthrough {
            self.drain(out, true);
        }
    }

    fn drain(&mut self, out: &mut Vec<f32>, last: bool) {
        let width = 2 * self.half + 1;
        loop {
            let pos = self.next_out * self.step;
            let (centre, phase) = (pos / self.phases, pos % self.phases);
            if centre >= self.seen {
                break;
            }
            // Mid-stream, wait until the whole filter fits; at the end the
            // missing tail counts as silence.
            if !last && centre + self.half >= self.seen {
                break;
            }
            let sample = if self.taps.is_empty() {
                self.linear(centre, phase)
            } else {
                let row = &self.taps[phase * width..(phase + 1) * width];
                self.convolve(centre, row)
            };
            out.push(sample);
            self.next_out += 1;
        }
        // Keep only what the next output can still reach back to.
        let next_centre = self.next_out * self.step / self.phases;
        let keep_from = next_centre.saturating_sub(self.half).max(self.buf_start);
        if keep_from > self.buf_start {
            self.buf.drain(..(keep_from - self.buf_start).min(self.buf.len()));
            self.buf_start = keep_from;
        }
    }

    fn convolve(&self, centre: usize, row: &[f32]) -> f32 {
        let first = centre as isize - self.half as isize;
        let lo = first - self.buf_start as isize;
        if lo >= 0 && lo as usize + row.len() <= self.buf.len() {
            let window = &self.buf[lo as usize..lo as usize + row.len()];
            return window.iter().zip(row).map(|(s, t)| s * t).sum();
        }
        // Start or end of the recording: use the taps that land on real audio
        // and renormalise, so the edges don't fade towards silence.
        let (mut acc, mut weight) = (0f32, 0f32);
        for (k, t) in row.iter().enumerate() {
            let idx = lo + k as isize;
            if idx >= 0 && (idx as usize) < self.buf.len() {
                acc += self.buf[idx as usize] * t;
                weight += t;
            }
        }
        if weight.abs() > 1e-6 { acc / weight } else { 0.0 }
    }

    /// Fallback for rates whose ratio needs an absurd number of phases.
    fn linear(&self, centre: usize, phase: usize) -> f32 {
        let at = |i: usize| {
            i.checked_sub(self.buf_start)
                .and_then(|j| self.buf.get(j))
                .copied()
        };
        let a = at(centre).unwrap_or(0.0);
        let b = at(centre + 1).unwrap_or(a);
        let frac = phase as f32 / self.phases as f32;
        a * (1.0 - frac) + b * frac
    }
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

/// Hann window over `x` in [-1, 1].
fn hann(x: f64) -> f64 {
    if x.abs() >= 1.0 {
        0.0
    } else {
        0.5 * (1.0 + (std::f64::consts::PI * x).cos())
    }
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
    fn dead_air_ignores_a_call_that_hung_up_before_stop() {
        let mut samples = tone(60 * 28, 16_000);
        samples.extend(std::iter::repeat_n(0.0, 16_000 * 104));
        assert_eq!(dead_air(&samples, 16_000), None);
    }

    #[test]
    fn dead_air_still_flags_a_tap_that_died_and_never_came_back() {
        let mut samples = tone(60 * 10, 16_000);
        samples.extend(std::iter::repeat_n(0.0, 16_000 * 60 * 15));
        let (at, _) = dead_air(&samples, 16_000).expect("15 dead minutes at the end");
        assert!((at - 600.0).abs() < 1.0);
    }

    fn sine(hz: f64, rate: u32, start: usize, len: usize) -> Vec<f32> {
        (start..start + len)
            .map(|i| (2.0 * std::f64::consts::PI * hz * i as f64 / rate as f64).sin() as f32 * 0.5)
            .collect()
    }

    /// Signal-to-noise of `out` against the ideal 16 kHz sine, in dB.
    fn snr_db(out: &[f32], hz: f64, first_out_index: usize) -> f64 {
        let (mut sig, mut err) = (0f64, 0f64);
        for (k, o) in out.iter().enumerate() {
            let i = first_out_index + k;
            let ideal = (2.0 * std::f64::consts::PI * hz * i as f64 / 16_000.0).sin() * 0.5;
            sig += ideal * ideal;
            err += (*o as f64 - ideal).powi(2);
        }
        10.0 * (sig / err.max(1e-30)).log10()
    }

    /// The bug that turned the second half of long meetings into noise: read
    /// positions computed in f32 drift off once the sample count passes 2^24.
    /// Feeds the resampler as if an hour had already gone by and checks the
    /// tone still comes out clean.
    #[test]
    fn resample_stays_exact_an_hour_into_a_recording() {
        for rate in [48_000u32, 24_000, 44_100] {
            let mut r = Resampler::new(rate);
            // Pretend 61 minutes were already consumed: positions are what
            // matter, and they depend only on the output index.
            let skipped_out = 61 * 60 * 16_000usize;
            let skipped_src = skipped_out * r.step / r.phases;
            r.next_out = skipped_out;
            r.buf_start = skipped_src;
            r.seen = skipped_src;
            let mut out = Vec::new();
            r.push(&sine(1_000.0, rate, skipped_src, rate as usize * 2), &mut out);
            // Skip the first taps, which see the artificial edge.
            let body = &out[200..out.len() - 200];
            let snr = snr_db(body, 1_000.0, skipped_out + 200);
            assert!(snr > 60.0, "{rate} Hz: SNR {snr:.1} dB an hour in");
        }
    }

    #[test]
    fn resample_keeps_speech_band_and_removes_what_would_alias() {
        // 3 kHz is speech and must pass; 11 kHz would fold onto 5 kHz.
        let pass = resample_to_16k(&sine(3_000.0, 48_000, 0, 48_000), 48_000);
        assert!(snr_db(&pass[400..pass.len() - 400], 3_000.0, 400) > 50.0);

        let stop = resample_to_16k(&sine(11_000.0, 48_000, 0, 48_000), 48_000);
        let rms = (stop[400..stop.len() - 400].iter().map(|s| s * s).sum::<f32>()
            / (stop.len() - 800) as f32)
            .sqrt();
        // The input sine has RMS 0.354; anything left is aliasing.
        assert!(rms < 0.354 * 0.01, "11 kHz leaked through at RMS {rms}");
    }

    #[test]
    fn streaming_in_odd_chunks_matches_one_shot() {
        let src = sine(700.0, 44_100, 0, 44_100);
        let whole = resample_to_16k(&src, 44_100);
        let mut r = Resampler::new(44_100);
        let mut out = Vec::new();
        for chunk in src.chunks(977) {
            r.push(chunk, &mut out);
        }
        r.finish(&mut out);
        assert_eq!(out.len(), whole.len());
        assert!(out.iter().zip(&whole).all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[test]
    fn silence_stays_exactly_silent() {
        // `dead_air` looks for exact zeros after resampling.
        let out = resample_to_16k(&vec![0.0; 48_000], 48_000);
        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn load_wav_16k_mono_downmixes_and_resamples() {
        let dir = std::env::temp_dir().join(format!("stt-md-wavload-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stereo.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for s in sine(440.0, 48_000, 0, 48_000) {
            let v = (s * i16::MAX as f32) as i16;
            w.write_sample(v).unwrap();
            w.write_sample(v).unwrap();
        }
        w.finalize().unwrap();
        let out = load_wav_16k_mono(&path).unwrap();
        assert!((out.len() as i64 - 16_000).abs() <= 2, "len={}", out.len());
        assert!(snr_db(&out[400..out.len() - 400], 440.0, 400) > 50.0);
        let _ = std::fs::remove_dir_all(&dir);
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
        assert!(out.iter().all(|v| (v - 0.5).abs() < 1e-5));
    }
}
