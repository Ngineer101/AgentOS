//! Voice input: a local speech-to-text model on the host.
//!
//! The model is NVIDIA's Nemotron 3.5 ASR (0.6B, streaming, multilingual),
//! as exported to ONNX by sherpa-onnx, run on the CPU in this process. It is
//! downloaded once, when a user first asks for voice input, into
//! `<data>/models/` (about 475 MB, checked against a pinned SHA-256). The
//! desktop and paired phones either stream audio while you talk (the text so
//! far comes back with each piece) or send a finished recording (WAV) as an
//! upload; nothing leaves the machine.

use crate::bus::{Bus, BusMsg};
use agenthost_proto::jsonrpc::{codes, RpcError};
use agenthost_proto::*;
use anyhow::{anyhow, bail, Context, Result};
use sha2::Digest;
use std::io::{Read, Write};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct ModelSpec {
    pub name: &'static str,
    /// Folder under `<data>/models`.
    pub dir: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
    pub encoder: &'static str,
    pub decoder: &'static str,
    pub joiner: &'static str,
    pub tokens: &'static str,
}

pub const MODEL: ModelSpec = ModelSpec {
    name: "NVIDIA Nemotron 3.5 ASR",
    dir: "nemotron-3.5-asr-streaming-0.6b-560ms-int8",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-560ms-int8-2026-06-11.tar.bz2",
    sha256: "c6bf5e0df765f9d5b43bc9e0536d4b4b3e7d40bdf5ecf13e45f134c51c05ae3a",
    bytes: 475_271_763,
    encoder: "encoder.int8.onnx",
    decoder: "decoder.int8.onnx",
    joiner: "joiner.int8.onnx",
    tokens: "tokens.txt",
};

/// Longest recording accepted.
const MAX_SECONDS: f32 = 10.0 * 60.0;
/// Live streams open at once, across all connections.
const MAX_STREAMS: usize = 8;
/// A live stream nobody fed for this long is dropped.
const STREAM_IDLE: Duration = Duration::from_secs(60);

/// One live dictation.
struct Live {
    stream: engine::Live,
    rate: u32,
    /// Samples received so far.
    samples: u64,
    touched: Instant,
}

#[derive(Default)]
struct State {
    progress: Option<VoiceProgress>,
    error: Option<String>,
}

pub struct Voice {
    models_dir: PathBuf,
    bus: Bus,
    http: reqwest::Client,
    state: Mutex<State>,
    /// Loaded on first use, kept for later recordings.
    engine: Mutex<Option<Arc<engine::Engine>>>,
    /// Live dictations by stream id.
    live: Mutex<HashMap<String, Arc<Mutex<Live>>>>,
}

impl Voice {
    pub fn new(data_dir: &Path, bus: Bus) -> Arc<Voice> {
        Arc::new(Voice {
            models_dir: data_dir.join("models"),
            bus,
            http: reqwest::Client::builder()
                .user_agent(concat!("agenthost/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(std::time::Duration::from_secs(20))
                .build()
                .expect("http client"),
            state: Mutex::new(State::default()),
            engine: Mutex::new(None),
            live: Mutex::new(HashMap::new()),
        })
    }

    pub fn model_dir(&self) -> PathBuf {
        self.models_dir.join(MODEL.dir)
    }

    fn installed(&self) -> bool {
        let d = self.model_dir();
        [MODEL.encoder, MODEL.decoder, MODEL.joiner, MODEL.tokens]
            .iter()
            .all(|f| d.join(f).is_file())
    }

    pub fn status(&self) -> VoiceStatus {
        let st = self.state.lock().unwrap();
        let error = if !engine::AVAILABLE {
            Some("this build of agenthostd has no voice support".into())
        } else {
            st.error.clone()
        };
        VoiceStatus {
            model: MODEL.name.into(),
            download_bytes: MODEL.bytes,
            ready: engine::AVAILABLE && self.installed(),
            streaming: engine::AVAILABLE,
            progress: st.progress.clone(),
            error,
        }
    }

    fn publish(&self) {
        self.bus.publish(BusMsg::VoiceChanged(self.status()));
    }

    /// Start downloading the model unless it is installed or already coming.
    pub fn download(self: &Arc<Self>) -> VoiceStatus {
        if !engine::AVAILABLE || self.installed() {
            return self.status();
        }
        {
            let mut st = self.state.lock().unwrap();
            if st.progress.is_some() {
                drop(st);
                return self.status();
            }
            st.progress = Some(VoiceProgress {
                received: 0,
                total: MODEL.bytes,
            });
            st.error = None;
        }
        let me = self.clone();
        tokio::spawn(async move {
            let r = me.fetch().await;
            {
                let mut st = me.state.lock().unwrap();
                st.progress = None;
                if let Err(e) = &r {
                    st.error = Some(format!("download failed: {e:#}"));
                }
            }
            match r {
                Ok(()) => tracing::info!(model = MODEL.name, "voice model installed"),
                Err(e) => tracing::warn!("voice model download failed: {e:#}"),
            }
            me.publish();
        });
        self.publish();
        self.status()
    }

    async fn fetch(&self) -> Result<()> {
        crate::config::create_private_dir(&self.models_dir)?;
        let archive = self.models_dir.join(format!(".{}.tar.bz2.part", MODEL.dir));
        let mut resp = self
            .http
            .get(MODEL.url)
            .send()
            .await?
            .error_for_status()
            .context("model download")?;
        let total = resp.content_length().unwrap_or(MODEL.bytes);
        let mut file = std::fs::File::create(&archive)?;
        let mut hash = sha2::Sha256::new();
        let mut received = 0u64;
        let mut last_pct = 0;
        while let Some(chunk) = resp.chunk().await? {
            file.write_all(&chunk)?;
            hash.update(&chunk);
            received += chunk.len() as u64;
            if received > MODEL.bytes + 1024 * 1024 {
                bail!("the download is larger than expected");
            }
            let pct = received * 100 / total.max(1);
            if pct != last_pct {
                last_pct = pct;
                self.state.lock().unwrap().progress = Some(VoiceProgress { received, total });
                self.publish();
            }
        }
        file.sync_all()?;
        drop(file);
        let got = hex::encode(hash.finalize());
        if got != MODEL.sha256 {
            let _ = std::fs::remove_file(&archive);
            bail!("checksum mismatch (got {got})");
        }
        let target = self.model_dir();
        let staging = self.models_dir.join(format!(".{}.extract", MODEL.dir));
        let (a, s) = (archive.clone(), staging.clone());
        tokio::task::spawn_blocking(move || extract(&a, &s))
            .await
            .map_err(|e| anyhow!("{e}"))??;
        let _ = std::fs::remove_file(&archive);
        let _ = std::fs::remove_dir_all(&target);
        std::fs::rename(&staging, &target)?;
        Ok(())
    }

    /// Transcribe a WAV file. Runs on a blocking thread; one at a time.
    pub async fn transcribe(
        self: &Arc<Self>,
        wav: &Path,
        language: Option<String>,
    ) -> Result<String, RpcError> {
        self.usable()?;
        let me = self.clone();
        let wav = wav.to_path_buf();
        tokio::task::spawn_blocking(move || -> Result<String> {
            let (rate, samples) = read_wav(&wav)?;
            let engine = me.engine()?;
            engine.transcribe(rate, &samples, language.as_deref())
        })
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?
        .map_err(|e| RpcError::invalid_params(format!("could not transcribe: {e:#}")))
    }

    fn usable(&self) -> Result<(), RpcError> {
        if !engine::AVAILABLE {
            return Err(RpcError::new(
                codes::UNAVAILABLE,
                "this build of agenthostd has no voice support",
            ));
        }
        if !self.installed() {
            return Err(RpcError::new(
                codes::UNAVAILABLE,
                "the voice model is not installed",
            )
            .with_data(serde_json::json!({"needs_download": true})));
        }
        Ok(())
    }

    /// Open a live dictation. Loads the model the first time (a second or two).
    pub async fn stream_begin(self: &Arc<Self>, p: VoiceStreamBeginParams) -> Result<String, RpcError> {
        self.usable()?;
        if !(8000..=192_000).contains(&p.sample_rate) {
            return Err(RpcError::invalid_params("sample_rate must be 8000 to 192000 Hz"));
        }
        {
            let mut live = self.live.lock().unwrap();
            // A stream busy decoding is in use; don't wait for it.
            live.retain(|_, l| match l.try_lock() {
                Ok(l) => l.touched.elapsed() < STREAM_IDLE,
                Err(std::sync::TryLockError::WouldBlock) => true,
                Err(std::sync::TryLockError::Poisoned(_)) => false,
            });
            if live.len() >= MAX_STREAMS {
                return Err(RpcError::new(codes::UNAVAILABLE, "too many dictations at once; try again in a moment"));
            }
        }
        let me = self.clone();
        let stream = tokio::task::spawn_blocking(move || -> Result<engine::Live> {
            let engine = me.engine()?;
            Ok(engine.live(p.language.as_deref()))
        })
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?
        .map_err(|e| RpcError::internal(format!("{e:#}")))?;
        let id = format!("vs_{}", uuid::Uuid::new_v4().simple());
        let l = Live { stream, rate: p.sample_rate, samples: 0, touched: Instant::now() };
        self.live.lock().unwrap().insert(id.clone(), Arc::new(Mutex::new(l)));
        Ok(id)
    }

    fn stream(&self, id: &str) -> Result<Arc<Mutex<Live>>, RpcError> {
        self.live
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| RpcError::invalid_params("no such voice stream (it may have been idle too long)"))
    }

    /// Feed a live dictation; returns the text so far.
    pub async fn stream_append(self: &Arc<Self>, p: VoiceStreamAppendParams) -> Result<String, RpcError> {
        let l = self.stream(&p.stream_id)?;
        let pcm = decode_pcm(&p.data)?;
        let r = feed(l, pcm, false).await;
        if r.is_err() {
            self.live.lock().unwrap().remove(&p.stream_id);
        }
        r
    }

    /// End a live dictation; returns the final text (empty when cancelled).
    pub async fn stream_finish(self: &Arc<Self>, p: VoiceStreamFinishParams) -> Result<String, RpcError> {
        let l = self
            .live
            .lock()
            .unwrap()
            .remove(&p.stream_id)
            .ok_or_else(|| RpcError::invalid_params("no such voice stream (it may have been idle too long)"))?;
        if p.cancel {
            return Ok(String::new());
        }
        let pcm = match &p.data {
            Some(d) => decode_pcm(d)?,
            None => vec![],
        };
        feed(l, pcm, true).await
    }

    fn engine(&self) -> Result<Arc<engine::Engine>> {
        let mut slot = self.engine.lock().unwrap();
        if let Some(e) = slot.as_ref() {
            return Ok(e.clone());
        }
        let d = self.model_dir();
        let e = Arc::new(engine::Engine::load(&d)?);
        *slot = Some(e.clone());
        Ok(e)
    }
}

/// Mono 16-bit little-endian PCM from base64, as floats.
fn decode_pcm(b64: &str) -> Result<Vec<f32>, RpcError> {
    use base64::Engine as _;
    if b64.len() > VOICE_STREAM_CHUNK_BYTES / 3 * 4 + 4 {
        return Err(RpcError::invalid_params("audio piece too large"));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|_| RpcError::invalid_params("audio is not valid base64"))?;
    Ok(bytes
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
        .collect())
}

/// Add audio to a live stream on a blocking thread; the text so far.
async fn feed(l: Arc<Mutex<Live>>, pcm: Vec<f32>, last: bool) -> Result<String, RpcError> {
    tokio::task::spawn_blocking(move || -> Result<String, RpcError> {
        let mut l = l.lock().unwrap();
        l.samples += pcm.len() as u64;
        l.touched = Instant::now();
        if l.samples as f32 / l.rate as f32 > MAX_SECONDS {
            return Err(RpcError::invalid_params(format!(
                "the dictation is longer than {} minutes",
                MAX_SECONDS as u32 / 60
            )));
        }
        let rate = l.rate;
        let r = if last { l.stream.finish(rate, &pcm) } else { l.stream.feed(rate, &pcm) };
        r.map_err(|e| RpcError::internal(format!("could not transcribe: {e:#}")))
    })
    .await
    .map_err(|e| RpcError::internal(e.to_string()))?
}

/// Unpack only the model's four files, flat, into `dest`.
fn extract(archive: &Path, dest: &Path) -> Result<()> {
    let _ = std::fs::remove_dir_all(dest);
    crate::config::create_private_dir(dest)?;
    let wanted = [MODEL.encoder, MODEL.decoder, MODEL.joiner, MODEL.tokens];
    let f = std::fs::File::open(archive)?;
    let mut tar = tar::Archive::new(bzip2::read::BzDecoder::new(std::io::BufReader::new(f)));
    let mut found = 0;
    for entry in tar.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path()?.into_owned();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Only the top folder's files (not test_wavs/), by exact name.
        if path.components().count() > 2 || !wanted.contains(&name) {
            continue;
        }
        let mut out = std::fs::File::create(dest.join(name))?;
        std::io::copy(&mut entry, &mut out)?;
        out.sync_all()?;
        found += 1;
    }
    if found != wanted.len() {
        bail!("the archive is missing model files");
    }
    Ok(())
}

/// Mono f32 samples from a PCM or float WAV, any rate.
pub fn read_wav(path: &Path) -> Result<(u32, Vec<f32>)> {
    let mut bytes = vec![];
    std::fs::File::open(path)?.read_to_end(&mut bytes)?;
    let mut r = hound::WavReader::new(std::io::Cursor::new(bytes)).context("not a WAV file")?;
    let spec = r.spec();
    if spec.channels == 0 || spec.sample_rate < 8000 {
        bail!("unsupported WAV format");
    }
    let n = r.duration() as f32 / spec.sample_rate as f32;
    if n > MAX_SECONDS {
        bail!("the recording is longer than {} minutes", MAX_SECONDS as u32 / 60);
    }
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()?
        }
    };
    let ch = spec.channels as usize;
    let mono = if ch == 1 {
        interleaved
    } else {
        interleaved
            .chunks(ch)
            .map(|f| f.iter().sum::<f32>() / ch as f32)
            .collect()
    };
    Ok((spec.sample_rate, mono))
}

#[cfg(feature = "voice")]
mod engine {
    use super::MODEL;
    use anyhow::{anyhow, Result};
    use std::path::Path;
    use std::sync::Arc;

    pub const AVAILABLE: bool = true;

    pub struct Engine {
        rec: sherpa_onnx::OnlineRecognizer,
        lock: std::sync::Mutex<()>,
    }

    impl Engine {
        pub fn load(dir: &Path) -> Result<Engine> {
            let f = |n: &str| Some(dir.join(n).to_string_lossy().into_owned());
            let mut c = sherpa_onnx::OnlineRecognizerConfig::default();
            c.model_config.transducer.encoder = f(MODEL.encoder);
            c.model_config.transducer.decoder = f(MODEL.decoder);
            c.model_config.transducer.joiner = f(MODEL.joiner);
            c.model_config.tokens = f(MODEL.tokens);
            c.model_config.num_threads = std::thread::available_parallelism()
                .map(|n| n.get().clamp(1, 8) as i32)
                .unwrap_or(4);
            let t = std::time::Instant::now();
            let rec = sherpa_onnx::OnlineRecognizer::create(&c)
                .ok_or_else(|| anyhow!("could not load the voice model"))?;
            tracing::info!(ms = t.elapsed().as_millis() as u64, "voice model loaded");
            Ok(Engine {
                rec,
                lock: std::sync::Mutex::new(()),
            })
        }

        pub fn transcribe(self: &Arc<Self>, rate: u32, samples: &[f32], language: Option<&str>) -> Result<String> {
            self.live(language).finish(rate, samples)
        }

        /// A stream to feed audio as it is recorded.
        pub fn live(self: &Arc<Self>, language: Option<&str>) -> Live {
            let s = self.rec.create_stream();
            if let Some(l) = language.filter(|l| l.len() <= 8 && l.chars().all(|c| c.is_ascii_alphabetic() || c == '-')) {
                s.set_option("language", l);
            }
            Live { s, engine: self.clone() }
        }
    }

    /// A streaming recognition. The stream is dropped before the engine.
    pub struct Live {
        s: sherpa_onnx::OnlineStream,
        engine: Arc<Engine>,
    }

    impl Live {
        /// Add audio and decode what can be decoded; the text so far.
        pub fn feed(&self, rate: u32, samples: &[f32]) -> Result<String> {
            let e = &self.engine;
            let _one = e.lock.lock().unwrap();
            if !samples.is_empty() {
                self.s.accept_waveform(rate as i32, samples);
            }
            while e.rec.is_ready(&self.s) {
                e.rec.decode(&self.s);
            }
            Ok(self.text())
        }

        /// Add the last audio and end the stream; the final text.
        pub fn finish(&self, rate: u32, samples: &[f32]) -> Result<String> {
            let e = &self.engine;
            let _one = e.lock.lock().unwrap();
            if !samples.is_empty() {
                self.s.accept_waveform(rate as i32, samples);
            }
            // Trailing silence lets the streaming model emit the last words.
            self.s.accept_waveform(rate as i32, &vec![0.0; rate as usize]);
            self.s.input_finished();
            while e.rec.is_ready(&self.s) {
                e.rec.decode(&self.s);
            }
            Ok(self.text())
        }

        fn text(&self) -> String {
            self.engine.rec.get_result(&self.s).map(|r| r.text.trim().to_string()).unwrap_or_default()
        }
    }
}

#[cfg(not(feature = "voice"))]
mod engine {
    use anyhow::{bail, Result};
    use std::path::Path;

    pub const AVAILABLE: bool = false;

    pub struct Engine;

    impl Engine {
        pub fn load(_: &Path) -> Result<Engine> {
            bail!("no voice support in this build")
        }
        pub fn transcribe(self: &std::sync::Arc<Self>, _: u32, _: &[f32], _: Option<&str>) -> Result<String> {
            bail!("no voice support in this build")
        }
        pub fn live(self: &std::sync::Arc<Self>, _: Option<&str>) -> Live {
            Live
        }
    }

    pub struct Live;

    impl Live {
        pub fn feed(&self, _: u32, _: &[f32]) -> Result<String> {
            bail!("no voice support in this build")
        }
        pub fn finish(&self, _: u32, _: &[f32]) -> Result<String> {
            bail!("no voice support in this build")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_wav_formats() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&p, spec).unwrap();
        for _ in 0..100 {
            w.write_sample(i16::MAX).unwrap();
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();
        let (rate, s) = read_wav(&p).unwrap();
        assert_eq!(rate, 44100);
        assert_eq!(s.len(), 100);
        assert!((s[0] - 0.5).abs() < 0.01, "{}", s[0]);
        std::fs::write(&p, b"not a wav").unwrap();
        assert!(read_wav(&p).is_err());
    }

    /// With AGENTHOST_TEST_VOICE_MODEL=<folder with the model files> and
    /// AGENTHOST_TEST_VOICE_WAV=<speech.wav>, transcribe for real.
    #[cfg(feature = "voice")]
    #[test]
    fn transcribes_with_the_real_model() {
        let (Ok(m), Ok(w)) = (
            std::env::var("AGENTHOST_TEST_VOICE_MODEL"),
            std::env::var("AGENTHOST_TEST_VOICE_WAV"),
        ) else {
            eprintln!("skipped: set AGENTHOST_TEST_VOICE_MODEL and AGENTHOST_TEST_VOICE_WAV");
            return;
        };
        let e = engine::Engine::load(Path::new(&m)).unwrap();
        let (rate, s) = read_wav(Path::new(&w)).unwrap();
        let e = Arc::new(e);
        let text = e.transcribe(rate, &s, None).unwrap();
        eprintln!("{text}");
        assert!(!text.is_empty());
        // Live, in 200 ms pieces: words come before the end, and the end matches.
        let live = e.live(None);
        let mut partial = String::new();
        for piece in s.chunks(rate as usize / 5) {
            let t = live.feed(rate, piece).unwrap();
            if partial.is_empty() && !t.is_empty() {
                partial = t;
            }
        }
        assert!(!partial.is_empty(), "no text before the end");
        assert_eq!(live.finish(rate, &[]).unwrap(), text);
    }

    /// The live API as clients call it, with the real model (same variables as above).
    #[cfg(feature = "voice")]
    #[tokio::test(flavor = "multi_thread")]
    async fn streams_live_dictation_with_the_real_model() {
        use base64::Engine as _;
        let (Ok(m), Ok(w)) = (
            std::env::var("AGENTHOST_TEST_VOICE_MODEL"),
            std::env::var("AGENTHOST_TEST_VOICE_WAV"),
        ) else {
            eprintln!("skipped: set AGENTHOST_TEST_VOICE_MODEL and AGENTHOST_TEST_VOICE_WAV");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let v = Voice::new(dir.path(), Bus::new());
        let dest = v.model_dir();
        std::fs::create_dir_all(&dest).unwrap();
        for f in [MODEL.encoder, MODEL.decoder, MODEL.joiner, MODEL.tokens] {
            std::os::unix::fs::symlink(Path::new(&m).join(f), dest.join(f)).unwrap();
        }
        assert!(v.status().ready && v.status().streaming);
        let (rate, s) = read_wav(Path::new(&w)).unwrap();
        let pcm = |s: &[f32]| {
            let b: Vec<u8> = s.iter().flat_map(|v| ((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
            base64::engine::general_purpose::STANDARD.encode(b)
        };
        let t = std::time::Instant::now();
        let id = v.stream_begin(VoiceStreamBeginParams { sample_rate: rate, language: None }).await.unwrap();
        eprintln!("begin (loads the model): {} ms", t.elapsed().as_millis());
        let mut first = None;
        let mut slowest = Duration::ZERO;
        for (i, piece) in s.chunks(rate as usize / 5).enumerate() {
            let t = std::time::Instant::now();
            let text = v.stream_append(VoiceStreamAppendParams { stream_id: id.clone(), data: pcm(piece) }).await.unwrap();
            slowest = slowest.max(t.elapsed());
            if first.is_none() && !text.is_empty() {
                first = Some(((i + 1) as f32 * 0.2, text));
            }
        }
        let t = std::time::Instant::now();
        let fin = v.stream_finish(VoiceStreamFinishParams { stream_id: id.clone(), data: None, cancel: false }).await.unwrap();
        let (at, words) = first.expect("no words before the end");
        eprintln!(
            "first words {words:?} after {at:.1} s of {:.1} s; slowest piece {} ms; finish {} ms; final: {fin}",
            s.len() as f32 / rate as f32,
            slowest.as_millis(),
            t.elapsed().as_millis()
        );
        assert!(!fin.is_empty());
        // Finished streams are gone; cancelled ones give nothing back.
        assert!(v.stream_append(VoiceStreamAppendParams { stream_id: id, data: String::new() }).await.is_err());
        let id = v.stream_begin(VoiceStreamBeginParams { sample_rate: rate, language: None }).await.unwrap();
        let r = v.stream_finish(VoiceStreamFinishParams { stream_id: id, data: Some(pcm(&s)), cancel: true }).await.unwrap();
        assert!(r.is_empty());
        assert!(v.live.lock().unwrap().is_empty());
    }
}
