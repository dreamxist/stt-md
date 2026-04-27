use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, TryRecvError};
use std::collections::VecDeque;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::audio_utils::resample_to_16k;

pub struct MixerHandle {
    pub handle: JoinHandle<Result<()>>,
}

/// Reads from `mic_rx` (potentially multi-channel) and `sys_rx` (mono f32),
/// downmixes/resamples both to a common 48kHz mono stream, mixes by addition
/// with clipping protection, and forwards combined Vec<f32> chunks to `out_tx`.
///
/// `mic_sample_rate` and `mic_channels` describe the cpal mic stream.
/// `sys_sample_rate` is fixed at 48kHz from SCStream (we configure it that way).
///
/// Drift handling: we consume the minimum available from both buffers each pass.
/// If one source stalls, the other accumulates in its buffer; this produces a
/// silent gap rather than a hang. For a 30-min meeting drift stays under ~100ms.
pub fn spawn_mixer(
    mic_rx: Receiver<Vec<f32>>,
    sys_rx: Receiver<Vec<f32>>,
    mic_sample_rate: u32,
    mic_channels: u16,
    sys_sample_rate: u32,
    out_tx: Sender<Vec<f32>>,
) -> MixerHandle {
    let handle = thread::spawn(move || -> Result<()> {
        let mut mic_buf: VecDeque<f32> = VecDeque::new();
        let mut sys_buf: VecDeque<f32> = VecDeque::new();
        let mut mic_closed = false;
        let mut sys_closed = false;

        loop {
            // Drain whatever is available without blocking. Detect Disconnected
            // (all Senders dropped) and stop polling that side.
            if !mic_closed {
                loop {
                    match mic_rx.try_recv() {
                        Ok(chunk) => {
                            let mono = downmix_to_mono(&chunk, mic_channels);
                            let resampled = resample(&mono, mic_sample_rate, 48_000);
                            mic_buf.extend(resampled);
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            mic_closed = true;
                            break;
                        }
                    }
                }
            }
            if !sys_closed {
                loop {
                    match sys_rx.try_recv() {
                        Ok(chunk) => {
                            let resampled = resample(&chunk, sys_sample_rate, 48_000);
                            sys_buf.extend(resampled);
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            sys_closed = true;
                            break;
                        }
                    }
                }
            }

            // While both sides are alive, mix only the overlapping portion
            // (so we never get ahead of either source). Once one side closes,
            // treat its missing samples as silence so we don't strand the
            // surviving side's audio in its buffer.
            let n = if mic_closed || sys_closed {
                mic_buf.len().max(sys_buf.len())
            } else {
                mic_buf.len().min(sys_buf.len())
            };

            if n > 0 {
                let mut mixed = Vec::with_capacity(n);
                for _ in 0..n {
                    let m = mic_buf.pop_front().unwrap_or(0.0);
                    let s = sys_buf.pop_front().unwrap_or(0.0);
                    mixed.push((m + s).clamp(-1.0, 1.0));
                }
                if out_tx.send(mixed).is_err() {
                    return Ok(());
                }
            }

            // Both sides closed and drained → finalize.
            if mic_closed && sys_closed && mic_buf.is_empty() && sys_buf.is_empty() {
                return Ok(());
            }

            // Avoid busy-spin when there is nothing to mix.
            if n == 0 {
                thread::sleep(Duration::from_millis(10));
            }
        }
    });
    MixerHandle { handle }
}

fn downmix_to_mono(samples: &[f32], channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }
    let ch = channels as usize;
    samples
        .chunks_exact(ch)
        .map(|frame| frame.iter().sum::<f32>() / ch as f32)
        .collect()
}

fn resample(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate {
        return samples.to_vec();
    }
    if to_rate == 16_000 {
        return resample_to_16k(samples, from_rate);
    }
    // Linear interpolation, same approach as audio_utils::resample_to_16k.
    let ratio = to_rate as f32 / from_rate as f32;
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
