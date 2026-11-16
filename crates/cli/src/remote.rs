//! The device side of remote access: pair with a QR payload, then connect to
//! the host through the relay. The CLI uses this to act as a phone; the iOS
//! app does the same through the shared tunnel crate.

use crate::Client;
use agenthost_proto::jsonrpc::Notification;
use agenthost_proto::tunnel::{
    self, b64, unb64, Handshake, PairHello, PairResult, PairingQr, RelayKey, StaticKey,
};
use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMsg;

/// Everything a paired device keeps. Private keys: keep this file private.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceState {
    pub relay_url: String,
    pub relay_id: String,
    pub host_key: String,
    pub host_name: String,
    pub device_id: String,
    pub host_id: String,
    pub noise_private: String,
    pub relay_private: String,
}

impl DeviceState {
    pub fn load(p: &Path) -> Result<DeviceState> {
        Ok(serde_json::from_str(
            &std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?,
        )?)
    }
    pub fn save(&self, p: &Path) -> Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(p)?;
        f.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        Ok(())
    }
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Why the relay or host closed us.
#[derive(Debug)]
pub struct Closed {
    pub code: u16,
    pub reason: String,
}
impl std::fmt::Display for Closed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self.code {
            4401 => "This device was removed",
            4403 => "not allowed (unknown device or used/expired pairing code)",
            4404 => "the host is offline",
            _ => "connection closed",
        };
        write!(
            f,
            "{what} (code {}{})",
            self.code,
            if self.reason.is_empty() {
                String::new()
            } else {
                format!(": {}", self.reason)
            }
        )
    }
}
impl std::error::Error for Closed {}

async fn next_text(ws: &mut Ws) -> Result<Value> {
    loop {
        match tokio::time::timeout(Duration::from_secs(20), ws.next())
            .await
            .map_err(|_| anyhow!("relay timed out"))?
        {
            Some(Ok(WsMsg::Text(t))) => return Ok(serde_json::from_str(&t)?),
            Some(Ok(WsMsg::Close(f))) => {
                let (code, reason) = f
                    .map(|f| (u16::from(f.code), f.reason.to_string()))
                    .unwrap_or((1000, String::new()));
                return Err(Closed { code, reason }.into());
            }
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(e.into()),
            None => bail!("relay closed"),
        }
    }
}

async fn next_binary(ws: &mut Ws, timeout: Duration) -> Result<Vec<u8>> {
    loop {
        match tokio::time::timeout(timeout, ws.next())
            .await
            .map_err(|_| anyhow!("timed out waiting for the host"))?
        {
            Some(Ok(WsMsg::Binary(b))) => return Ok(b.to_vec()),
            Some(Ok(WsMsg::Close(f))) => {
                let (code, reason) = f
                    .map(|f| (u16::from(f.code), f.reason.to_string()))
                    .unwrap_or((1000, String::new()));
                return Err(Closed { code, reason }.into());
            }
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(e.into()),
            None => bail!("relay closed"),
        }
    }
}

/// Install ring as the process-wide rustls crypto provider. Without it, a
/// wss:// connect panics when no single provider is enabled by features
/// (as in the iOS library). A no-op if a provider is already installed.
fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

async fn open(url: &str) -> Result<(Ws, String)> {
    ensure_crypto_provider();
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .with_context(|| format!("connect to relay {url}"))?;
    let ch = next_text(&mut ws).await?;
    let nonce = ch["nonce"]
        .as_str()
        .ok_or_else(|| anyhow!("no challenge"))?
        .to_string();
    Ok((ws, nonce))
}

async fn await_ready(ws: &mut Ws) -> Result<()> {
    loop {
        let v = next_text(ws).await?;
        if v["type"] == "ready" {
            return Ok(());
        }
    }
}

/// Pair with a host. `on_code` is called with the 6-digit code to compare.
pub async fn pair(qr_text: &str, name: &str, on_code: impl FnOnce(&str)) -> Result<DeviceState> {
    let qr = PairingQr::decode(qr_text)?;
    let noise = StaticKey::generate();
    let relay_key = RelayKey::generate();
    let url = format!(
        "{}/v1/relay/{}/pair",
        qr.relay_url.trim_end_matches('/'),
        qr.relay_id
    );
    let (mut ws, _nonce) = open(&url).await?;
    let token = tunnel::pairing_token(&qr.secret);
    ws.send(WsMsg::Text(
        json!({"type":"auth","role":"pair","pairing_token": b64(&token)}).to_string(),
    ))
    .await?;
    await_ready(&mut ws).await?;
    let mut hs = Handshake::pair_initiator(&noise, &qr)?;
    ws.send(WsMsg::Binary(hs.write(&[])?)).await?;
    hs.read(&next_binary(&mut ws, Duration::from_secs(20)).await?)?;
    let hello = PairHello {
        name: name.to_string(),
        relay_key: b64(&relay_key.public()),
    };
    ws.send(WsMsg::Binary(hs.write(&serde_json::to_vec(&hello)?)?))
        .await?;
    on_code(&tunnel::short_code(&hs.handshake_hash()));
    let mut t = hs.into_transport()?;
    // Wait for the user to confirm on the desktop.
    let result: PairResult = loop {
        let f = next_binary(&mut ws, Duration::from_secs(150)).await?;
        if let Some(m) = t.open(&f)? {
            break serde_json::from_slice(&m)?;
        }
    };
    let _ = ws.close(None).await;
    if !result.accepted {
        bail!("pairing refused: {}", result.reason.unwrap_or_default());
    }
    Ok(DeviceState {
        relay_url: qr.relay_url,
        relay_id: qr.relay_id,
        host_key: b64(&qr.host_key),
        host_name: qr.host_name,
        device_id: result.device_id.unwrap_or_default(),
        host_id: result.host_id.unwrap_or_default(),
        noise_private: b64(&noise.private),
        relay_private: b64(&relay_key.to_bytes()),
    })
}

/// Connect to the host through the relay and speak the client protocol.
/// The returned receiver ends when the tunnel closes; `closed` then says why.
pub async fn connect(
    state: &DeviceState,
) -> Result<(
    Client,
    mpsc::Receiver<Notification>,
    tokio::sync::watch::Receiver<Option<String>>,
)> {
    let noise = StaticKey::from_private(&unb64(&state.noise_private)?)?;
    let relay_key = RelayKey::from_bytes(&unb64(&state.relay_private)?)?;
    let host_key: [u8; 32] = unb64(&state.host_key)?
        .try_into()
        .map_err(|_| anyhow!("bad host key"))?;
    let url = format!(
        "{}/v1/relay/{}/device",
        state.relay_url.trim_end_matches('/'),
        state.relay_id
    );
    let (mut ws, nonce) = open(&url).await?;
    let sig = relay_key.sign_challenge("device", &state.relay_id, &nonce);
    ws.send(WsMsg::Text(json!({"type":"auth","role":"device","public_key": b64(&relay_key.public()),"signature": sig}).to_string())).await?;
    await_ready(&mut ws).await?;
    let mut hs = Handshake::connect_initiator(&noise, &host_key)?;
    ws.send(WsMsg::Binary(hs.write(&[])?)).await?;
    hs.read(&next_binary(&mut ws, Duration::from_secs(20)).await?)?;
    let mut transport = hs.into_transport()?;

    let (out_tx, mut out_rx) = mpsc::channel::<String>(256);
    let (in_tx, in_rx) = mpsc::channel::<String>(4096);
    let (closed_tx, closed_rx) = tokio::sync::watch::channel::<Option<String>>(None);
    let (mut sink, mut stream) = ws.split();
    tokio::spawn(async move {
        let reason = loop {
            tokio::select! {
                line = out_rx.recv() => {
                    let Some(line) = line else { break "client closed".to_string() };
                    match transport.seal(line.as_bytes()) {
                        Ok(frames) => {
                            for f in frames {
                                if sink.send(WsMsg::Binary(f)).await.is_err() { break; }
                            }
                        }
                        Err(e) => break format!("tunnel error: {e}"),
                    }
                }
                msg = stream.next() => {
                    match msg {
                        Some(Ok(WsMsg::Binary(b))) => match transport.open(&b) {
                            Ok(Some(m)) => {
                                let Ok(line) = String::from_utf8(m) else { break "bad message".into() };
                                if in_tx.send(line).await.is_err() { break "client closed".into(); }
                            }
                            Ok(None) => {}
                            Err(e) => {
                                let _ = sink.send(WsMsg::Close(None)).await;
                                break format!("tunnel integrity failure: {e}");
                            }
                        },
                        Some(Ok(WsMsg::Close(f))) => {
                            let (code, reason) = f.map(|f| (u16::from(f.code), f.reason.to_string())).unwrap_or((1000, String::new()));
                            break Closed { code, reason }.to_string();
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => break e.to_string(),
                        None => break "relay closed".into(),
                    }
                }
            }
        };
        let _ = closed_tx.send(Some(reason));
    });
    let (c, n) = Client::over(out_tx, in_rx, "agenthost-cli-remote").await?;
    Ok((c, n, closed_rx))
}
