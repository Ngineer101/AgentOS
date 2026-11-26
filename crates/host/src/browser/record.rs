//! Recordings of an agent's browser tab, sent to the user as an MP4.
//!
//! While a session records, its tab's screencast runs (whether or not
//! anyone watches in the Browser panel) and every frame Chromium sends is
//! offered here. Chromium sends a frame only when the page changes, so the
//! video has a variable frame rate: each picture lasts until the next one.
//! A thread of its own decodes the JPEGs, encodes them as H.264 (at most
//! [`FPS`] a second, the newest frame winning) and keeps the result in
//! memory; stopping writes the MP4 ([`crate::mp4`]).
//!
//! A recording is at most [`MAX_LENGTH`] long and stays under the
//! attachment limit: past either, it keeps what it has and ignores the rest.

use crate::mp4;
use anyhow::{anyhow, Result};
use base64::Engine;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, RateControlMode, UsageType};
use openh264::formats::{RgbSliceU8, YUVBuffer};
use openh264::OpenH264API;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// The longest recording.
pub const MAX_LENGTH: Duration = Duration::from_secs(3 * 60);
/// Most pictures a second.
pub const FPS: f32 = 10.0;
/// What the encoder aims for: about 20 MB for the longest recording.
const BITRATE: u32 = 900_000;
/// The video's largest side; bigger pages are scaled down.
const MAX_SIDE: u32 = 1600;
/// Past this many bytes of video it stops taking frames (the attachment
/// limit, less room for the index).
const MAX_BYTES: usize = agenthost_proto::MAX_ATTACHMENT_BYTES as usize - 512 * 1024;
/// How long the last picture shows at least.
const LAST_FRAME: Duration = Duration::from_millis(500);

/// A finished recording.
pub struct Finished {
    pub mp4: Vec<u8>,
    /// The last picture, as the JPEG Chromium sent, to show before it plays.
    pub poster: Option<Vec<u8>>,
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    /// It hit [`MAX_LENGTH`] or the size limit, so the end is missing.
    pub cut: bool,
}

#[derive(Default)]
struct Slot {
    /// The newest frame not yet encoded (base64 JPEG) and when it came.
    latest: Option<(Instant, Arc<str>)>,
    stop: Option<Instant>,
    /// Throw it away (the session ended).
    discard: bool,
}

/// Where the browser's frames go. Cheap to offer to: it never blocks on
/// the encoder.
pub struct Sink {
    slot: Mutex<Slot>,
    wake: Condvar,
}

impl Sink {
    pub fn offer(&self, jpeg_b64: Arc<str>) {
        let mut s = self.slot.lock().unwrap();
        if s.stop.is_none() {
            s.latest = Some((Instant::now(), jpeg_b64));
            self.wake.notify_one();
        }
    }
}

/// A recording in progress.
pub struct Recorder {
    pub sink: Arc<Sink>,
    pub started: Instant,
    thread: Option<std::thread::JoinHandle<Result<Finished>>>,
}

impl Recorder {
    /// Start recording; `first` is the picture showing now, if known.
    pub fn start(first: Option<Arc<str>>) -> Result<Recorder> {
        let started = Instant::now();
        let sink = Arc::new(Sink { slot: Mutex::new(Slot { latest: first.map(|f| (started, f)), ..Default::default() }), wake: Condvar::new() });
        let encoder = new_encoder()?;
        let s = sink.clone();
        let thread = std::thread::Builder::new().name("browser-record".into()).spawn(move || run(s, encoder, started))?;
        Ok(Recorder { sink, started, thread: Some(thread) })
    }

    /// Stop and write the MP4. Blocks while the last frame encodes.
    pub fn finish(mut self) -> Result<Finished> {
        {
            let mut s = self.sink.slot.lock().unwrap();
            s.stop.get_or_insert_with(Instant::now);
            self.sink.wake.notify_one();
        }
        let t = self.thread.take().ok_or_else(|| anyhow!("the recording already finished"))?;
        t.join().map_err(|_| anyhow!("the recorder crashed"))?
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let mut s = self.sink.slot.lock().unwrap();
            s.discard = true;
            s.stop.get_or_insert_with(Instant::now);
            self.sink.wake.notify_one();
        }
    }
}

fn new_encoder() -> Result<Encoder> {
    let cfg = EncoderConfig::new()
        .usage_type(UsageType::ScreenContentRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .bitrate(BitRate::from_bps(BITRATE))
        .max_frame_rate(FrameRate::from_hz(FPS))
        // It holds the bitrate by skipping pictures when it must (the one
        // before then shows longer); a key frame every 5 s for seeking.
        .skip_frames(true)
        // Not supported for screen content (it says so, then turns them off).
        .adaptive_quantization(false)
        .background_detection(false)
        .intra_frame_period(IntraFramePeriod::from_num_frames(5 * FPS as u32));
    Encoder::with_api_config(OpenH264API::from_source(), cfg).map_err(|e| anyhow!("could not start the video encoder: {e}"))
}

/// The encoder's side: what it has made so far.
struct Track {
    encoder: Encoder,
    size: Option<(u32, u32)>,
    sps: Vec<u8>,
    pps: Vec<u8>,
    samples: Vec<mp4::Sample>,
    /// When each sample starts, from the first.
    starts: Vec<Duration>,
    first_at: Option<Instant>,
    bytes: usize,
    poster: Option<Arc<str>>,
    cut: bool,
}

fn run(sink: Arc<Sink>, encoder: Encoder, started: Instant) -> Result<Finished> {
    let gap = Duration::from_secs_f32(1.0 / FPS);
    let mut t = Track { encoder, size: None, sps: vec![], pps: vec![], samples: vec![], starts: vec![], first_at: None, bytes: 0, poster: None, cut: false };
    let mut last_encode: Option<Instant> = None;
    loop {
        let (frame, stop) = {
            let mut s = sink.slot.lock().unwrap();
            while s.latest.is_none() && s.stop.is_none() {
                // Wake at the length limit even if the page stays still.
                let left = (started + MAX_LENGTH).saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                s = sink.wake.wait_timeout(s, left).unwrap().0;
            }
            if s.discard {
                return Err(anyhow!("the recording was thrown away"));
            }
            // At most FPS pictures a second: wait, and take the newest.
            if s.stop.is_none() {
                if let Some(at) = last_encode {
                    let since = at.elapsed();
                    if since < gap {
                        drop(s);
                        std::thread::sleep(gap - since);
                        continue;
                    }
                }
            }
            (s.latest.take(), s.stop)
        };
        if let Some((at, data)) = frame {
            if at.saturating_duration_since(started) > MAX_LENGTH || t.bytes > MAX_BYTES {
                t.cut = true;
            } else if let Err(e) = encode(&mut t, at, &data) {
                tracing::debug!("browser recording: a frame was skipped: {e:#}");
            }
            last_encode = Some(Instant::now());
        } else if stop.is_none() && started.elapsed() > MAX_LENGTH {
            t.cut = true;
        }
        if let Some(end) = stop {
            return finish(t, end.min(started + MAX_LENGTH));
        }
        if t.cut {
            // Nothing more goes in; wait for the stop.
            let mut s = sink.slot.lock().unwrap();
            while s.stop.is_none() {
                s = sink.wake.wait(s).unwrap();
            }
            if s.discard {
                return Err(anyhow!("the recording was thrown away"));
            }
            let end = s.stop.unwrap();
            drop(s);
            return finish(t, end.min(started + MAX_LENGTH));
        }
    }
}

fn encode(t: &mut Track, at: Instant, b64: &str) -> Result<()> {
    let jpeg = base64::engine::general_purpose::STANDARD.decode(b64.as_bytes())?;
    let (w, h, rgb) = decode_jpeg(&jpeg).ok_or_else(|| anyhow!("not a JPEG"))?;
    let (ow, oh) = *t.size.get_or_insert_with(|| fit(w, h));
    let rgb = if (w, h) == (ow, oh) { rgb } else { scale_into(&rgb, w, h, ow, oh) };
    let yuv = YUVBuffer::from_rgb8_source(RgbSliceU8::new(&rgb, (ow as usize, oh as usize)));
    let bs = t.encoder.encode(&yuv).map_err(|e| anyhow!("{e}"))?;
    let mut data = vec![];
    let mut sync = false;
    for l in 0..bs.num_layers() {
        let Some(layer) = bs.layer(l) else { continue };
        for n in 0..layer.nal_count() {
            let Some(unit) = layer.nal_unit(n) else { continue };
            for nal in mp4::nal_units(unit) {
                match mp4::nal_type(nal) {
                    7 => t.sps = nal.to_vec(),
                    8 => t.pps = nal.to_vec(),
                    k => {
                        sync |= k == 5;
                        data.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                        data.extend_from_slice(nal);
                    }
                }
            }
        }
    }
    if data.is_empty() {
        // Skipped: the picture before shows longer.
        return Ok(());
    }
    if t.samples.is_empty() && !sync {
        return Err(anyhow!("the first picture is not a key frame"));
    }
    let first = *t.first_at.get_or_insert(at);
    let start = at.saturating_duration_since(first).max(t.starts.last().copied().unwrap_or_default());
    t.bytes += data.len();
    t.samples.push(mp4::Sample { data, duration: 0, sync });
    t.starts.push(start);
    t.poster = Some(Arc::from(b64));
    Ok(())
}

fn finish(mut t: Track, end: Instant) -> Result<Finished> {
    let Some(first) = t.first_at else {
        return Err(anyhow!("nothing was recorded: the tab showed no picture"));
    };
    if t.sps.is_empty() || t.pps.is_empty() {
        return Err(anyhow!("the encoder gave no stream parameters"));
    }
    let end = end.saturating_duration_since(first).max(*t.starts.last().unwrap() + LAST_FRAME);
    for i in 0..t.samples.len() {
        let next = t.starts.get(i + 1).copied().unwrap_or(end);
        t.samples[i].duration = next.saturating_sub(t.starts[i]).as_millis().max(1) as u32;
    }
    let duration_ms = t.samples.iter().map(|s| s.duration as u64).sum();
    let (w, h) = t.size.unwrap_or_default();
    let mp4 = mp4::write(&mp4::Track { width: w as u16, height: h as u16, timescale: 1000, sps: &t.sps, pps: &t.pps }, &t.samples);
    let poster = t.poster.and_then(|p| base64::engine::general_purpose::STANDARD.decode(p.as_bytes()).ok());
    Ok(Finished { mp4, poster, duration_ms, width: w, height: h, cut: t.cut })
}

/// A JPEG as RGB pixels, and its size.
fn decode_jpeg(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB).set_max_width(8192).set_max_height(8192);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(data), opts);
    let px = d.decode().ok()?;
    let (w, h) = d.dimensions()?;
    (px.len() == w * h * 3).then_some((w as u32, h as u32, px))
}

/// The video's size for a page of `w`×`h`: at most [`MAX_SIDE`], even
/// (H.264's colour planes are half size).
fn fit(w: u32, h: u32) -> (u32, u32) {
    let scale = (MAX_SIDE as f64 / w.max(h).max(1) as f64).min(1.0);
    let even = |v: f64| ((v.round() as u32) & !1).max(2);
    (even(w as f64 * scale), even(h as f64 * scale))
}

/// A picture of another size (the page was resized) fitted into the
/// video's, centred on black.
fn scale_into(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; (dw * dh * 3) as usize];
    let scale = (dw as f64 / sw as f64).min(dh as f64 / sh as f64);
    let (fw, fh) = (((sw as f64 * scale) as u32).clamp(1, dw), ((sh as f64 * scale) as u32).clamp(1, dh));
    let (ox, oy) = ((dw - fw) / 2, (dh - fh) / 2);
    for y in 0..fh {
        let sy = ((y as f64 / scale) as u32).min(sh - 1);
        for x in 0..fw {
            let sx = ((x as f64 / scale) as u32).min(sw - 1);
            let s = ((sy * sw + sx) * 3) as usize;
            let d = (((oy + y) * dw + ox + x) * 3) as usize;
            out[d..d + 3].copy_from_slice(&src[s..s + 3]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_even_and_capped() {
        assert_eq!(fit(1280, 800), (1280, 800));
        assert_eq!(fit(1281, 801), (1280, 800));
        assert_eq!(fit(3200, 2000), (1600, 1000));
        assert_eq!(fit(1, 1), (2, 2));
    }

    /// A `w`×`h` JPEG, grey with a white square at `x`, as base64.
    fn frame(w: u16, h: u16, x: u16) -> Arc<str> {
        let mut px = vec![60u8; w as usize * h as usize * 3];
        for yy in 20..60.min(h) {
            for xx in x..(x + 40).min(w) {
                let i = (yy as usize * w as usize + xx as usize) * 3;
                px[i..i + 3].copy_from_slice(&[255, 255, 255]);
            }
        }
        let mut out = vec![];
        jpeg_encoder::Encoder::new(&mut out, 80).encode(&px, w, h, jpeg_encoder::ColorType::Rgb).unwrap();
        Arc::from(base64::engine::general_purpose::STANDARD.encode(out))
    }

    #[test]
    fn frames_become_an_mp4_that_ffprobe_reads() {
        let r = Recorder::start(Some(frame(320, 200, 0))).unwrap();
        for i in 1..=12u16 {
            std::thread::sleep(Duration::from_millis(60));
            r.sink.offer(frame(320, 200, i * 20));
        }
        // The page is resized: still the same video size.
        std::thread::sleep(Duration::from_millis(150));
        r.sink.offer(frame(400, 200, 100));
        std::thread::sleep(Duration::from_millis(300));
        let f = r.finish().unwrap();
        assert_eq!((f.width, f.height), (320, 200));
        assert!(!f.cut);
        // About a second and a quarter, plus the last picture's half second.
        assert!((1_300..2_600).contains(&f.duration_ms), "{} ms", f.duration_ms);
        assert!(f.poster.as_ref().is_some_and(|p| p.starts_with(&[0xff, 0xd8])), "the poster is a JPEG");
        assert_eq!(&f.mp4[4..8], b"ftyp");

        let Ok(probe) = std::process::Command::new("ffprobe").arg("-version").output() else {
            eprintln!("no ffprobe here: the file's layout is checked by mp4's own tests only");
            return;
        };
        assert!(probe.status.success());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.mp4");
        std::fs::write(&path, &f.mp4).unwrap();
        let out = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries", "stream=codec_name,width,height,nb_read_frames,duration", "-of", "default=nw=1"])
            .arg(&path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && out.stderr.is_empty(), "{text}");
        assert!(text.contains("codec_name=h264") && text.contains("width=320") && text.contains("height=200"), "{text}");
        let frames: u32 = text.lines().find_map(|l| l.strip_prefix("nb_read_frames=")).unwrap().parse().unwrap();
        assert!((5..=14).contains(&frames), "{frames} frames: at most 10 a second, the newest winning\n{text}");
        let secs: f64 = text.lines().find_map(|l| l.strip_prefix("duration=")).unwrap().parse().unwrap();
        assert!((secs * 1000.0 - f.duration_ms as f64).abs() < 50.0, "{text}");
    }

    #[test]
    fn nothing_shown_is_an_error_not_an_empty_video() {
        let r = Recorder::start(None).unwrap();
        let Err(e) = r.finish() else { panic!("a video of nothing") };
        assert!(e.to_string().contains("nothing was recorded"), "{e}");
    }

    #[test]
    fn a_resized_page_is_letterboxed() {
        // 2×1 white into 4×4: a 4×2 white band in the middle.
        let out = scale_into(&[255; 6], 2, 1, 4, 4);
        let row = |y: usize| &out[y * 12..y * 12 + 12];
        assert!(row(0).iter().all(|&v| v == 0) && row(3).iter().all(|&v| v == 0));
        assert!(row(1).iter().all(|&v| v == 255) && row(2).iter().all(|&v| v == 255));
    }
}
