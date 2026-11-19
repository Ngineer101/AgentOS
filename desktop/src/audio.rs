//! Microphone recording for voice input: the default input device, kept in
//! memory, streamed to the host in pieces while you talk ([`Recorder::take_new`]),
//! or handed over as a 16-bit mono WAV at the end for hosts without live dictation.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Stop by itself after this long (the host accepts 10 minutes).
pub const MAX_SECONDS: u64 = 9 * 60;

pub struct Recorder {
    _stream: cpal::Stream,
    buf: Arc<Mutex<Vec<f32>>>,
    /// Loudest sample since the last `level()`, for the meter.
    peak: Arc<Mutex<f32>>,
    rate: u32,
    channels: u16,
    pub started: Instant,
}

impl Recorder {
    pub fn start() -> Result<Recorder, String> {
        let host = cpal::default_host();
        let dev = host
            .default_input_device()
            .ok_or("No microphone found. Connect one, or check the system's sound settings.")?;
        let cfg = dev.default_input_config().map_err(|e| format!("The microphone can't be used: {e}"))?;
        let rate = cfg.sample_rate().0;
        let channels = cfg.channels();
        let buf = Arc::new(Mutex::new(Vec::<f32>::with_capacity(rate as usize * 30)));
        let peak = Arc::new(Mutex::new(0f32));
        let err = |e: cpal::StreamError| eprintln!("microphone: {e}");
        let stream_cfg: cpal::StreamConfig = cfg.clone().into();
        let cap = rate as usize * channels as usize * MAX_SECONDS as usize;
        macro_rules! build {
            ($t:ty, $conv:expr) => {{
                let (b, p) = (buf.clone(), peak.clone());
                dev.build_input_stream(
                    &stream_cfg,
                    move |data: &[$t], _: &_| {
                        let mut b = b.lock().unwrap();
                        let mut pk = 0f32;
                        for &s in data {
                            let v: f32 = $conv(s);
                            pk = pk.max(v.abs());
                            if b.len() < cap {
                                b.push(v);
                            }
                        }
                        let mut p = p.lock().unwrap();
                        *p = p.max(pk);
                    },
                    err,
                    None,
                )
            }};
        }
        let stream = match cfg.sample_format() {
            cpal::SampleFormat::F32 => build!(f32, |s: f32| s),
            cpal::SampleFormat::I16 => build!(i16, |s: i16| s as f32 / 32768.0),
            cpal::SampleFormat::U16 => build!(u16, |s: u16| (s as f32 - 32768.0) / 32768.0),
            cpal::SampleFormat::I32 => build!(i32, |s: i32| s as f32 / 2147483648.0),
            f => return Err(format!("The microphone's sample format ({f}) isn't supported.")),
        }
        .map_err(|e| format!("Could not open the microphone: {e}"))?;
        stream.play().map_err(|e| format!("Could not start the microphone: {e}"))?;
        Ok(Recorder { _stream: stream, buf, peak, rate, channels, started: Instant::now() })
    }

    pub fn seconds(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    /// Peak level since the last call, 0..1.
    pub fn level(&self) -> f32 {
        let mut p = self.peak.lock().unwrap();
        let v = *p;
        *p = 0.0;
        v.min(1.0)
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// The audio recorded since the last call, mono 16-bit PCM (little-endian bytes).
    pub fn take_new(&self) -> Vec<u8> {
        let ch = self.channels.max(1) as usize;
        let mut b = self.buf.lock().unwrap();
        let whole = b.len() - b.len() % ch;
        let frames: Vec<f32> = b.drain(..whole).collect();
        drop(b);
        let mut out = Vec::with_capacity(frames.len() / ch * 2);
        for f in frames.chunks(ch) {
            let v = f.iter().sum::<f32>() / ch as f32;
            out.extend_from_slice(&((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
        }
        out
    }

    /// Stop and return the recording as WAV bytes, or None if it is too short to hold speech.
    pub fn finish(self) -> Option<Vec<u8>> {
        let samples = std::mem::take(&mut *self.buf.lock().unwrap());
        drop(self._stream);
        let ch = self.channels.max(1) as usize;
        let mono: Vec<f32> = if ch == 1 { samples } else { samples.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect() };
        if mono.len() < self.rate as usize / 3 {
            return None;
        }
        Some(wav16(self.rate, &mono))
    }
}

/// A 16-bit PCM mono WAV file.
pub fn wav16(rate: u32, samples: &[f32]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    w
}
