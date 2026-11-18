//! Remote access: the relay connector, pairing and revocation.
//!
//! The host dials out to the relay (it never listens on the network) and
//! serves the client protocol to paired devices through Noise tunnels. The
//! relay only ever forwards ciphertext. See docs/relay-protocol.md.

use crate::api::{serve_connection, Principal};
use crate::bus::BusMsg;
use crate::ctx::Ctx;
use crate::host::{DeviceAdmin, Host};
use crate::secrets::SecretStore;
use crate::store::DeviceRecord;
use crate::util::{new_id, now_ms};
use agenthost_proto::jsonrpc::{codes, RpcError};
use agenthost_proto::methods::{self as m, Capability};
use agenthost_proto::tunnel::{
    self, b64, unb64, Handshake, PairHello, PairResult, PairingQr, RelayKey, StaticKey, Transport,
};
use agenthost_proto::*;
use anyhow::{anyhow, bail, Context as _, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as WsMsg;

const NOISE_SECRET: &str = "host-noise-key";
const RELAY_SECRET: &str = "host-relay-key";

enum Out {
    Text(String),
    Binary(Vec<u8>),
}

struct Pairing {
    pairing_id: String,
    secret: [u8; 32],
    expires_at: i64,
    used: bool,
    decision: Option<oneshot::Sender<bool>>,
}

/// How often the host pings the relay, and how long a silent link may last.
const RELAY_PING: Duration = Duration::from_secs(20);
const RELAY_SILENCE: Duration = Duration::from_secs(50);

pub struct Remote {
    host: Weak<Host>,
    ctx: Arc<Ctx>,
    noise: StaticKey,
    relay: RelayKey,
    pub relay_id: String,
    url: Option<String>,
    status: Mutex<RelayStatus>,
    out: Mutex<Option<mpsc::Sender<Out>>>,
    conns: Mutex<HashMap<String, mpsc::Sender<Vec<u8>>>>,
    pairing: Mutex<Option<Pairing>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

/// Load or create a key in the secret store.
fn load_key(
    secrets: &dyn SecretStore,
    name: &str,
    gen: impl FnOnce() -> Vec<u8>,
) -> Result<Vec<u8>> {
    if let Some(k) = secrets.get(name)? {
        return Ok(k);
    }
    let k = gen();
    secrets.set(name, &k)?;
    Ok(k)
}

fn relay_endpoint(base: &str, relay_id: &str, role: &str) -> String {
    format!("{}/v1/relay/{relay_id}/{role}", base.trim_end_matches('/'))
}

impl Remote {
    pub fn new(host: &Arc<Host>) -> Result<Arc<Remote>> {
        let secrets = host.secrets.as_ref();
        let noise = StaticKey::from_private(&load_key(secrets, NOISE_SECRET, || {
            StaticKey::generate().private.to_vec()
        })?)?;
        let relay = RelayKey::from_bytes(&load_key(secrets, RELAY_SECRET, || {
            RelayKey::generate().to_bytes().to_vec()
        })?)?;
        let relay_id = relay.relay_id();
        host.ctx
            .store
            .host_set("public_key", &hex::encode(noise.public))?;
        let url = host.ctx.cfg.relay_url.clone();
        Ok(Arc::new(Remote {
            host: Arc::downgrade(host),
            ctx: host.ctx.clone(),
            noise,
            relay,
            relay_id,
            status: Mutex::new(RelayStatus {
                configured: url.is_some(),
                connected: false,
                url: url.clone(),
                error: None,
            }),
            url,
            out: Mutex::new(None),
            conns: Mutex::new(HashMap::new()),
            pairing: Mutex::new(None),
            task: Mutex::new(None),
        }))
    }

    /// Keep a connection to the relay, reconnecting with backoff.
    pub fn spawn(self: &Arc<Self>) {
        let Some(url) = self.url.clone() else { return };
        let me = self.clone();
        let task = tokio::spawn(async move {
            let mut backoff = Duration::from_secs(1);
            loop {
                let started = std::time::Instant::now();
                match me.run_once(&url).await {
                    Ok(()) => {}
                    Err(e) => {
                        tracing::warn!("relay: {e:#}");
                        me.status.lock().unwrap().error = Some(format!("{e:#}"));
                    }
                }
                me.status.lock().unwrap().connected = false;
                *me.out.lock().unwrap() = None;
                me.conns.lock().unwrap().clear();
                if started.elapsed() > Duration::from_secs(30) {
                    backoff = Duration::from_secs(1);
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        });
        *self.task.lock().unwrap() = Some(task);
    }

    fn send(&self, o: Out) {
        if let Some(tx) = self.out.lock().unwrap().as_ref() {
            let _ = tx.try_send(o);
        }
    }

    fn control(&self, v: Value) {
        self.send(Out::Text(v.to_string()));
    }

    fn device_keys_b64(&self) -> Vec<String> {
        self.ctx
            .store
            .device_list(false)
            .unwrap_or_default()
            .iter()
            .filter_map(|d| hex::decode(&d.relay_key).ok().map(|k| b64(&k)))
            .collect()
    }

    async fn run_once(self: &Arc<Self>, url: &str) -> Result<()> {
        let endpoint = relay_endpoint(url, &self.relay_id, "host");
        let (ws, _) = tokio::time::timeout(
            Duration::from_secs(15),
            tokio_tungstenite::connect_async(endpoint.as_str()),
        )
        .await
        .map_err(|_| anyhow!("connect timed out"))?
        .with_context(|| format!("connect to {url}"))?;
        let (mut sink, mut stream) = ws.split();
        // Challenge / response.
        let challenge = loop {
            match stream.next().await {
                Some(Ok(WsMsg::Text(t))) => break serde_json::from_str::<Value>(&t)?,
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(e.into()),
                None => bail!("relay closed before challenge"),
            }
        };
        let nonce = challenge["nonce"]
            .as_str()
            .ok_or_else(|| anyhow!("no challenge from relay"))?
            .to_string();
        let sig = self.relay.sign_challenge("host", &self.relay_id, &nonce);
        sink.send(WsMsg::Text(json!({"type":"auth","role":"host","public_key": b64(&self.relay.public()),"signature": sig}).to_string())).await?;
        loop {
            match stream.next().await {
                Some(Ok(WsMsg::Text(t))) => {
                    let v: Value = serde_json::from_str(&t)?;
                    if v["type"] == "ready" {
                        break;
                    }
                }
                Some(Ok(WsMsg::Close(f))) => bail!("relay refused the host: {:?}", f),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
                None => bail!("relay closed during auth"),
            }
        }
        let (tx, mut rx) = mpsc::channel::<Out>(4096);
        *self.out.lock().unwrap() = Some(tx);
        {
            let mut st = self.status.lock().unwrap();
            st.connected = true;
            st.error = None;
        }
        tracing::info!(relay_id = %self.relay_id, "relay connected");
        self.control(json!({"type":"devices.set","public_keys": self.device_keys_b64()}));
        // Re-open a pairing slot that is still live.
        if let Some(p) = self
            .pairing
            .lock()
            .unwrap()
            .as_ref()
            .filter(|p| !p.used && p.expires_at > now_ms())
        {
            self.control(json!({"type":"pairing.open","token_hash": b64(&tunnel::sha256(&tunnel::pairing_token(&p.secret))),"expires_at": p.expires_at}));
        }
        let writer = tokio::spawn(async move {
            let mut ping = tokio::time::interval(RELAY_PING);
            loop {
                tokio::select! {
                    o = rx.recv() => {
                        let Some(o) = o else { break };
                        let msg = match o { Out::Text(t) => WsMsg::Text(t), Out::Binary(b) => WsMsg::Binary(b) };
                        if sink.send(msg).await.is_err() { break; }
                    }
                    _ = ping.tick() => {
                        if sink.send(WsMsg::Text(r#"{"type":"ping"}"#.to_string())).await.is_err() { break; }
                    }
                }
            }
        });
        // The relay answers every ping; silence this long means the link is
        // dead (sleep, network change) even if TCP hasn't noticed.
        loop {
            let msg = match tokio::time::timeout(RELAY_SILENCE, stream.next()).await {
                Err(_) => {
                    writer.abort();
                    bail!("the relay stopped answering; reconnecting");
                }
                Ok(None) => break,
                Ok(Some(m)) => m,
            };
            match msg? {
                WsMsg::Text(t) => {
                    let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                    match v["type"].as_str() {
                        Some("open") => {
                            let conn_id = v["conn_id"].as_str().unwrap_or_default().to_string();
                            let role = v["role"].as_str().unwrap_or_default().to_string();
                            self.open_conn(conn_id, role);
                        }
                        Some("closed") => {
                            let id = v["conn_id"].as_str().unwrap_or_default();
                            self.conns.lock().unwrap().remove(id);
                        }
                        Some("error") => tracing::warn!("relay error: {}", v["message"]),
                        _ => {}
                    }
                }
                WsMsg::Binary(b) => {
                    let Some((&n, rest)) = b.split_first() else {
                        continue;
                    };
                    let n = n as usize;
                    if rest.len() < n {
                        continue;
                    }
                    let conn_id = String::from_utf8_lossy(&rest[..n]).to_string();
                    let tx = self.conns.lock().unwrap().get(&conn_id).cloned();
                    if let Some(tx) = tx {
                        if tx.try_send(rest[n..].to_vec()).is_err() {
                            // Overflow or gone: close it.
                            self.close_conn(&conn_id, 4000);
                        }
                    }
                }
                WsMsg::Close(f) => {
                    tracing::warn!("relay closed the host socket: {:?}", f);
                    break;
                }
                _ => {}
            }
        }
        writer.abort();
        Ok(())
    }

    fn close_conn(&self, conn_id: &str, code: u16) {
        self.conns.lock().unwrap().remove(conn_id);
        self.control(json!({"type":"close","conn_id": conn_id, "code": code}));
    }

    fn send_frame(&self, conn_id: &str, payload: &[u8]) {
        let mut f = Vec::with_capacity(payload.len() + conn_id.len() + 1);
        f.push(conn_id.len() as u8);
        f.extend_from_slice(conn_id.as_bytes());
        f.extend_from_slice(payload);
        self.send(Out::Binary(f));
    }

    fn open_conn(self: &Arc<Self>, conn_id: String, role: String) {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(1024);
        self.conns.lock().unwrap().insert(conn_id.clone(), tx);
        let me = self.clone();
        tokio::spawn(async move {
            let r = match role.as_str() {
                "device" => me.serve_device(&conn_id, rx).await,
                "pair" => me.serve_pairing(&conn_id, rx).await,
                _ => Err(anyhow!("unknown role {role}")),
            };
            if let Err(e) = &r {
                tracing::info!(conn = %conn_id, "tunnel closed: {e:#}");
            }
            let code = match &r {
                Err(e) if e.to_string().contains("revoked") => 4401,
                Err(e) if e.to_string().contains("unknown device") => 4403,
                _ => 4000,
            };
            me.close_conn(&conn_id, code);
        });
    }

    /// A paired device: IK handshake, then the client protocol.
    async fn serve_device(
        self: &Arc<Self>,
        conn_id: &str,
        mut rx: mpsc::Receiver<Vec<u8>>,
    ) -> Result<()> {
        let mut hs = Handshake::connect_responder(&self.noise)?;
        let m1 = tokio::time::timeout(Duration::from_secs(15), rx.recv())
            .await?
            .ok_or_else(|| anyhow!("closed"))?;
        hs.read(&m1)?;
        let remote = hs.remote_static().ok_or_else(|| anyhow!("no device key"))?;
        let Some(rec) = self.ctx.store.device_by_key(&hex::encode(remote))? else {
            self.ctx
                .audit
                .log("tunnel.unknown_device", json!({"conn_id": conn_id}));
            bail!("unknown device key");
        };
        let m2 = hs.write(&[])?;
        self.send_frame(conn_id, &m2);
        let mut transport = hs.into_transport()?;
        let host = self.host.upgrade().ok_or_else(|| anyhow!("host gone"))?;
        let principal = Principal {
            device_id: Some(rec.device.device_id.clone()),
            capabilities: rec.device.capabilities.clone(),
            // The device id is in the audit entries next to the label.
            label: rec.device.name.clone(),
        };
        let _ = self.ctx.store.device_seen(&rec.device.device_id);
        let (in_tx, in_rx) = mpsc::channel::<String>(256);
        let (out_tx, mut out_rx) = mpsc::channel::<String>(1024);
        let serve = tokio::spawn(serve_connection(host, principal, in_rx, out_tx));
        let mut bus = self.ctx.bus.subscribe();
        let device_id = rec.device.device_id.clone();
        loop {
            tokio::select! {
                frame = rx.recv() => {
                    let Some(frame) = frame else { break };
                    // Any decryption failure (tampering, replay, reorder) closes the tunnel.
                    if let Some(msg) = transport.open(&frame).map_err(|e| anyhow!("tunnel integrity failure: {e}"))? {
                        let line = String::from_utf8(msg).map_err(|_| anyhow!("not UTF-8"))?;
                        if in_tx.send(line).await.is_err() { break; }
                    }
                }
                line = out_rx.recv() => {
                    let Some(line) = line else { break };
                    for f in transport.seal(line.as_bytes())? {
                        self.send_frame(conn_id, &f);
                    }
                }
                msg = bus.recv() => {
                    if let Ok(BusMsg::DeviceRevoked(id)) = msg {
                        if id == device_id {
                            // serve_connection sends device.revoked and exits; flush it.
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            while let Ok(line) = out_rx.try_recv() {
                                for f in transport.seal(line.as_bytes())? {
                                    self.send_frame(conn_id, &f);
                                }
                            }
                            serve.abort();
                            bail!("device revoked");
                        }
                    }
                }
            }
        }
        serve.abort();
        // The connection handler stops when the device is revoked; say so with 4401.
        if self.ctx.store.device_get(&device_id).ok().flatten().map(|d| d.device.revoked_at.is_some()).unwrap_or(false) {
            bail!("device revoked");
        }
        Ok(())
    }

    /// Pairing: XXpsk3 with the QR secret, then wait for the desktop to confirm.
    async fn serve_pairing(
        self: &Arc<Self>,
        conn_id: &str,
        mut rx: mpsc::Receiver<Vec<u8>>,
    ) -> Result<()> {
        let (secret, pairing_id) = {
            let mut p = self.pairing.lock().unwrap();
            let Some(p) = p.as_mut().filter(|p| !p.used && p.expires_at > now_ms()) else {
                bail!("no live pairing code")
            };
            p.used = true;
            (p.secret, p.pairing_id.clone())
        };
        let result: Result<()> = async {
            let mut hs = Handshake::pair_responder(&self.noise, &secret)?;
            hs.read(&recv_frame(&mut rx).await?)?;
            let m2 = hs.write(&[])?;
            self.send_frame(conn_id, &m2);
            let hello: PairHello = serde_json::from_slice(&hs.read(&recv_frame(&mut rx).await?)?)?;
            let device_key = hs.remote_static().ok_or_else(|| anyhow!("no device key"))?;
            let relay_key = unb64(&hello.relay_key)?;
            if relay_key.len() != 32 {
                bail!("bad device relay key");
            }
            let short_code = tunnel::short_code(&hs.handshake_hash());
            let mut transport: Transport = hs.into_transport()?;
            let name: String = hello.name.chars().filter(|c| !c.is_control()).take(64).collect();
            let (dtx, drx) = oneshot::channel();
            if let Some(p) = self.pairing.lock().unwrap().as_mut() {
                p.decision = Some(dtx);
            }
            self.ctx.bus.publish(BusMsg::PairingPending(PairingPending { pairing_id: pairing_id.clone(), device_name: name.clone(), short_code }));
            self.ctx.audit.log("pairing.pending", json!({"pairing_id": pairing_id, "device_name": name}));
            let accepted = tokio::time::timeout(Duration::from_secs(120), drx).await.ok().and_then(|r| r.ok()).unwrap_or(false);
            let result = if accepted {
                let device_id = new_id("dev");
                let rec = DeviceRecord {
                    device: Device { device_id: device_id.clone(), name: name.clone(), capabilities: Capability::phone(), created_at: now_ms(), last_seen_at: None, revoked_at: None },
                    public_key: hex::encode(device_key),
                    relay_key: hex::encode(&relay_key),
                };
                self.ctx.store.device_insert(&rec)?;
                self.control(json!({"type":"device.allow","public_key": b64(&relay_key)}));
                self.ctx.audit.log("pairing.accepted", json!({"device_id": device_id, "device_name": name}));
                PairResult { accepted: true, device_id: Some(device_id), host_id: Some(self.ctx.host_id.clone()), reason: None }
            } else {
                PairResult { accepted: false, device_id: None, host_id: None, reason: Some("refused on the host".into()) }
            };
            for f in transport.seal(&serde_json::to_vec(&result)?)? {
                self.send_frame(conn_id, &f);
            }
            self.ctx.bus.publish(BusMsg::PairingDone {
                pairing_id: pairing_id.clone(),
                result: json!({"ok": result.accepted, "device_id": result.device_id, "device_name": name, "reason": result.reason}),
            });
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok(())
        }
        .await;
        if let Err(e) = &result {
            self.ctx.bus.publish(BusMsg::PairingDone {
                pairing_id: pairing_id.clone(),
                result: json!({"ok": false, "reason": format!("{e:#}")}),
            });
        }
        *self.pairing.lock().unwrap() = None;
        result
    }

    fn pair_start(&self) -> Result<PairStartResult, RpcError> {
        let url = self.url.clone().ok_or_else(|| {
            RpcError::new(
                codes::UNAVAILABLE,
                "no relay configured; start the host with --relay-url",
            )
        })?;
        if !self.status.lock().unwrap().connected {
            return Err(RpcError::new(
                codes::UNAVAILABLE,
                "not connected to the relay yet",
            ));
        }
        let mut secret = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut secret);
        let expires_at = now_ms() + tunnel::PAIRING_TTL_MS;
        let pairing_id = new_id("pair");
        // One live code at a time: a new one replaces the old.
        if let Some(old) = self.pairing.lock().unwrap().take() {
            self.control(json!({"type":"pairing.close","token_hash": b64(&tunnel::sha256(&tunnel::pairing_token(&old.secret)))}));
        }
        self.control(json!({"type":"pairing.open","token_hash": b64(&tunnel::sha256(&tunnel::pairing_token(&secret))),"expires_at": expires_at}));
        let host_name = hostname();
        let qr = PairingQr {
            relay_url: url,
            relay_id: self.relay_id.clone(),
            host_key: self.noise.public,
            secret,
            host_name,
        };
        *self.pairing.lock().unwrap() = Some(Pairing {
            pairing_id: pairing_id.clone(),
            secret,
            expires_at,
            used: false,
            decision: None,
        });
        self.ctx
            .audit
            .log("pairing.start", json!({"pairing_id": pairing_id}));
        Ok(PairStartResult {
            pairing_id,
            qr_payload: qr.encode(),
            expires_at,
        })
    }
}

async fn recv_frame(rx: &mut mpsc::Receiver<Vec<u8>>) -> Result<Vec<u8>> {
    tokio::time::timeout(Duration::from_secs(30), rx.recv())
        .await
        .ok()
        .flatten()
        .ok_or_else(|| anyhow!("pairing timed out"))
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .unwrap_or_else(|| "host".into())
}

#[async_trait::async_trait]
impl DeviceAdmin for Remote {
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let p = if params.is_null() { json!({}) } else { params };
        let bad = |e: serde_json::Error| RpcError::invalid_params(e.to_string());
        let internal = |e: anyhow::Error| RpcError::internal(e.to_string());
        match method {
            m::DEVICE_LIST => {
                let list: Vec<Device> = self
                    .ctx
                    .store
                    .device_list(false)
                    .map_err(internal)?
                    .into_iter()
                    .map(|d| d.device)
                    .collect();
                Ok(serde_json::to_value(list).unwrap())
            }
            m::DEVICE_PAIR_START => Ok(serde_json::to_value(self.pair_start()?).unwrap()),
            m::DEVICE_PAIR_CONFIRM => {
                let c: PairConfirmParams = serde_json::from_value(p).map_err(bad)?;
                let tx = {
                    let mut g = self.pairing.lock().unwrap();
                    match g.as_mut() {
                        Some(pr) if pr.pairing_id == c.pairing_id => pr.decision.take(),
                        _ => None,
                    }
                };
                let tx = tx
                    .ok_or_else(|| RpcError::not_found("no pairing is waiting for confirmation"))?;
                let _ = tx.send(c.accept);
                Ok(json!({"ok": true}))
            }
            m::DEVICE_RENAME => {
                let r: DeviceRenameParams = serde_json::from_value(p).map_err(bad)?;
                if !self
                    .ctx
                    .store
                    .device_rename(&r.device_id, &r.name)
                    .map_err(internal)?
                {
                    return Err(RpcError::not_found("no such device"));
                }
                Ok(json!({"ok": true}))
            }
            m::DEVICE_REVOKE => {
                let id: IdParams = serde_json::from_value(p).map_err(bad)?;
                let rec = self
                    .ctx
                    .store
                    .device_get(&id.id)
                    .map_err(internal)?
                    .ok_or_else(|| RpcError::not_found("no such device"))?;
                if rec.device.revoked_at.is_some() {
                    return Ok(json!({"ok": true}));
                }
                self.ctx.store.device_revoke(&id.id).map_err(internal)?;
                // Tell the device first (through its tunnel), then have the
                // relay drop it and refuse it from now on.
                self.ctx.bus.publish(BusMsg::DeviceRevoked(id.id.clone()));
                if let Ok(k) = hex::decode(&rec.relay_key) {
                    let msg = json!({"type":"device.revoke","public_key": b64(&k)});
                    let out = self.out.lock().unwrap().clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        if let Some(tx) = out {
                            let _ = tx.send(Out::Text(msg.to_string())).await;
                        }
                    });
                }
                self.ctx
                    .audit
                    .log("device.revoked", json!({"device_id": id.id}));
                Ok(json!({"ok": true}))
            }
            _ => Err(RpcError::new(codes::METHOD_NOT_FOUND, method.to_string())),
        }
    }

    fn relay_status(&self) -> RelayStatus {
        self.status.lock().unwrap().clone()
    }

    fn shutdown(&self) {
        if let Some(t) = self.task.lock().unwrap().take() {
            t.abort();
        }
        *self.out.lock().unwrap() = None;
        self.status.lock().unwrap().connected = false;
    }
}
