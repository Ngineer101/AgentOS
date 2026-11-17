//! A development relay: the protocol in docs/relay-protocol.md, in process.
//!
//! Used by tests (with hooks to log, alter or replay frames) and by
//! `agenthostd dev-relay` for local end-to-end runs without Cloudflare. The
//! production relay is the Cloudflare Worker in `relay/`.

use agenthost_proto::tunnel::{b64, relay_id, unb64, verify_challenge};
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;

/// What a test relay does to frames it forwards.
#[derive(Default)]
pub struct Tamper {
    /// Every forwarded binary payload, as the relay saw it.
    pub log: Vec<Vec<u8>>,
    /// Flip a byte in the Nth device→host frame (counting from 1).
    pub flip_device_frame: Option<usize>,
    /// Send the Nth device→host frame twice.
    pub replay_device_frame: Option<usize>,
    /// Device→host frames forwarded so far.
    pub device_frames: usize,
    /// Go silent towards devices: forward nothing to or from them and answer
    /// no pings, like a network that died without closing the socket.
    pub stall_devices: bool,
}

enum Ctl {
    Msg(Message),
    Close(u16, String),
}

#[derive(Default)]
struct Room {
    host: Option<(u64, mpsc::UnboundedSender<Ctl>)>,
    devices: HashSet<String>,
    pairings: HashMap<String, i64>,
    /// conn_id -> (sender to device socket, device key if any)
    conns: HashMap<String, (mpsc::UnboundedSender<Ctl>, Option<String>)>,
}

#[derive(Default)]
struct State {
    rooms: HashMap<String, Room>,
    next: u64,
}

#[derive(Clone)]
pub struct DevRelay {
    pub addr: SocketAddr,
    state: Arc<Mutex<State>>,
    pub tamper: Arc<Mutex<Tamper>>,
}

impl DevRelay {
    pub fn url(&self) -> String {
        format!("ws://{}", self.addr)
    }

    pub async fn start(listen: &str) -> Result<DevRelay> {
        let l = TcpListener::bind(listen).await?;
        let r = DevRelay {
            addr: l.local_addr()?,
            state: Default::default(),
            tamper: Default::default(),
        };
        let rr = r.clone();
        tokio::spawn(async move {
            while let Ok((s, _)) = l.accept().await {
                let rr = rr.clone();
                tokio::spawn(async move {
                    let _ = rr.serve(s).await;
                });
            }
        });
        Ok(r)
    }

    async fn serve(&self, s: tokio::net::TcpStream) -> Result<()> {
        let mut path = String::new();
        let ws = tokio_tungstenite::accept_hdr_async(s, |req: &Request, resp: Response| {
            path = req.uri().path().to_string();
            Ok(resp)
        })
        .await?;
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        let (mut sink, mut stream) = ws.split();
        let close = |code: u16, reason: &str| {
            Message::Close(Some(CloseFrame {
                code: CloseCode::from(code),
                reason: reason.to_string().into(),
            }))
        };
        if parts.len() != 4 || parts[0] != "v1" || parts[1] != "relay" || parts[2].len() != 32 {
            let _ = sink.send(close(4400, "bad path")).await;
            return Ok(());
        }
        let rid = parts[2].to_string();
        let role = parts[3].to_string();
        let mut nonce_b = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce_b);
        let nonce = b64(&nonce_b);
        sink.send(Message::Text(
            json!({"type":"challenge","nonce":nonce}).to_string(),
        ))
        .await?;
        let auth = match tokio::time::timeout(Duration::from_secs(10), stream.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => {
                serde_json::from_str::<Value>(&t).unwrap_or(Value::Null)
            }
            Ok(_) => {
                let _ = sink.send(close(4400, "protocol")).await;
                return Ok(());
            }
            Err(_) => {
                let _ = sink.send(close(4408, "auth timeout")).await;
                return Ok(());
            }
        };
        if auth["type"] != "auth" || auth["role"] != role.as_str() {
            let _ = sink.send(close(4400, "protocol")).await;
            return Ok(());
        }
        let (tx, mut rx) = mpsc::unbounded_channel::<Ctl>();
        let conn_id = format!("{:016x}", rand::random::<u64>());
        let mut device_key = None;
        // Verify (no await while the lock is held).
        let fail: Option<u16> = {
            let mut st = self.state.lock().unwrap();
            st.next += 1;
            let my_id = st.next;
            let room = st.rooms.entry(rid.clone()).or_default();
            let fail = match role.as_str() {
                "host" => {
                    let pk = unb64(auth["public_key"].as_str().unwrap_or("")).unwrap_or_default();
                    let sig = unb64(auth["signature"].as_str().unwrap_or("")).unwrap_or_default();
                    let pk32: Option<[u8; 32]> = pk.as_slice().try_into().ok();
                    if pk32.map(|k| relay_id(&k) == rid).unwrap_or(false)
                        && verify_challenge(&pk, "host", &rid, &nonce, &sig)
                    {
                        if let Some((_, old)) = room.host.replace((my_id, tx.clone())) {
                            let _ = old.send(Ctl::Close(4409, "replaced".into()));
                        }
                        None
                    } else {
                        Some(4403)
                    }
                }
                "device" | "pair" if room.host.is_none() => Some(4404),
                "device" => {
                    let pk_s = auth["public_key"].as_str().unwrap_or("").to_string();
                    let pk = unb64(&pk_s).unwrap_or_default();
                    let sig = unb64(auth["signature"].as_str().unwrap_or("")).unwrap_or_default();
                    if room.devices.contains(&pk_s)
                        && verify_challenge(&pk, "device", &rid, &nonce, &sig)
                    {
                        device_key = Some(pk_s);
                        None
                    } else {
                        Some(4403)
                    }
                }
                "pair" => {
                    let tok =
                        unb64(auth["pairing_token"].as_str().unwrap_or("")).unwrap_or_default();
                    let h = b64(&agenthost_proto::tunnel::sha256(&tok));
                    match room.pairings.remove(&h) {
                        Some(exp) if exp > crate::util::now_ms() => None,
                        _ => Some(4403),
                    }
                }
                _ => Some(4400),
            };
            if fail.is_none() && role != "host" {
                room.conns
                    .insert(conn_id.clone(), (tx.clone(), device_key.clone()));
                if let Some((_, h)) = &room.host {
                    let _ = h.send(Ctl::Msg(Message::Text(
                        json!({"type":"open","conn_id":conn_id,"role":role,"public_key":device_key}).to_string(),
                    )));
                }
            }
            fail
        };
        if let Some(code) = fail {
            let _ = sink.send(close(code, "refused")).await;
            return Ok(());
        }
        sink.send(Message::Text(
            json!({"type":"ready","conn_id":conn_id}).to_string(),
        ))
        .await?;
        let writer = tokio::spawn(async move {
            while let Some(c) = rx.recv().await {
                match c {
                    Ctl::Msg(m) => {
                        if sink.send(m).await.is_err() {
                            break;
                        }
                    }
                    Ctl::Close(code, r) => {
                        let _ = sink
                            .send(Message::Close(Some(CloseFrame {
                                code: CloseCode::from(code),
                                reason: r.into(),
                            })))
                            .await;
                        break;
                    }
                }
            }
        });
        while let Some(Ok(msg)) = stream.next().await {
            match (role.as_str(), msg) {
                ("host", Message::Text(t)) => self.host_control(&rid, &t),
                ("host", Message::Binary(b)) => {
                    let Some((&n, rest)) = b.split_first() else {
                        continue;
                    };
                    if rest.len() < n as usize {
                        continue;
                    }
                    let cid = String::from_utf8_lossy(&rest[..n as usize]).to_string();
                    let payload = rest[n as usize..].to_vec();
                    if self.tamper.lock().unwrap().stall_devices {
                        continue;
                    }
                    self.tamper.lock().unwrap().log.push(payload.clone());
                    let st = self.state.lock().unwrap();
                    if let Some((d, _)) = st.rooms.get(&rid).and_then(|r| r.conns.get(&cid)) {
                        let _ = d.send(Ctl::Msg(Message::Binary(payload)));
                    }
                }
                (_, Message::Binary(_)) if self.tamper.lock().unwrap().stall_devices => {}
                (_, Message::Binary(b)) => {
                    let mut payloads = vec![b.to_vec()];
                    {
                        let mut t = self.tamper.lock().unwrap();
                        t.device_frames += 1;
                        let n = t.device_frames;
                        t.log.push(b.to_vec());
                        if t.flip_device_frame == Some(n) {
                            let i = payloads[0].len() / 2;
                            payloads[0][i] ^= 0x40;
                        }
                        if t.replay_device_frame == Some(n) {
                            payloads.push(b.to_vec());
                        }
                    }
                    let st = self.state.lock().unwrap();
                    if let Some((_, h)) = st.rooms.get(&rid).and_then(|r| r.host.as_ref()) {
                        for p in payloads {
                            let mut f = vec![conn_id.len() as u8];
                            f.extend_from_slice(conn_id.as_bytes());
                            f.extend_from_slice(&p);
                            let _ = h.send(Ctl::Msg(Message::Binary(f)));
                        }
                    }
                }
                (_, Message::Close(_)) => break,
                ("host", _) => {}
                // Keepalive, answered like the Worker's auto-response.
                (_, Message::Text(_)) if self.tamper.lock().unwrap().stall_devices => {}
                (_, Message::Text(t)) if t.as_str() == r#"{"type":"ping"}"# => {
                    let _ = tx.send(Ctl::Msg(Message::Text(r#"{"type":"pong"}"#.to_string())));
                }
                (_, Message::Text(_)) => break,
                _ => {}
            }
        }
        writer.abort();
        let mut st = self.state.lock().unwrap();
        if let Some(room) = st.rooms.get_mut(&rid) {
            if role == "host" {
                room.host = None;
                for (_, (d, _)) in room.conns.drain() {
                    let _ = d.send(Ctl::Close(4404, "host offline".into()));
                }
            } else if room.conns.remove(&conn_id).is_some() {
                if let Some((_, h)) = &room.host {
                    let _ = h.send(Ctl::Msg(Message::Text(
                        json!({"type":"closed","conn_id":conn_id}).to_string(),
                    )));
                }
            }
        }
        Ok(())
    }

    fn host_control(&self, rid: &str, t: &str) {
        let v: Value = serde_json::from_str(t).unwrap_or(Value::Null);
        let mut st = self.state.lock().unwrap();
        let Some(room) = st.rooms.get_mut(rid) else {
            return;
        };
        let close_key = |room: &mut Room, key: &str| {
            let ids: Vec<String> = room
                .conns
                .iter()
                .filter(|(_, (_, k))| k.as_deref() == Some(key))
                .map(|(c, _)| c.clone())
                .collect();
            for c in ids {
                if let Some((d, _)) = room.conns.remove(&c) {
                    let _ = d.send(Ctl::Close(4401, "revoked".into()));
                }
            }
        };
        match v["type"].as_str() {
            Some("devices.set") => {
                room.devices = v["public_keys"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|k| k.as_str().map(str::to_string))
                    .collect();
            }
            Some("device.allow") => {
                if let Some(k) = v["public_key"].as_str() {
                    room.devices.insert(k.to_string());
                }
            }
            Some("device.revoke") => {
                if let Some(k) = v["public_key"].as_str() {
                    room.devices.remove(k);
                    close_key(room, k);
                }
            }
            Some("pairing.open") => {
                if let (Some(h), Some(e)) = (v["token_hash"].as_str(), v["expires_at"].as_i64()) {
                    room.pairings.insert(h.to_string(), e);
                }
            }
            Some("pairing.close") => {
                if let Some(h) = v["token_hash"].as_str() {
                    room.pairings.remove(h);
                }
            }
            Some("close") => {
                if let Some(c) = v["conn_id"].as_str() {
                    if let Some((d, _)) = room.conns.remove(c) {
                        let code = v["code"].as_u64().unwrap_or(4000) as u16;
                        let _ = d.send(Ctl::Close(code, "closed by host".into()));
                    }
                }
            }
            Some("ping") => {
                if let Some((_, h)) = &room.host {
                    let _ = h.send(Ctl::Msg(Message::Text(r#"{"type":"pong"}"#.to_string())));
                }
            }
            _ => {}
        }
    }
}
