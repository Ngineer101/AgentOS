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
    /// Webhooks: the key deliveries are sealed to, the allowed hook hashes,
    /// and sealed deliveries the host hasn't acked (by delivery id, in order).
    seal_key: Option<[u8; 32]>,
    hooks: HashSet<String>,
    hook_queue: std::collections::BTreeMap<String, String>,
}

/// Largest webhook body the relay takes.
pub const MAX_HOOK_BODY: usize = 262_144;

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
                    let mut head = [0u8; 5];
                    let n = s.peek(&mut head).await.unwrap_or(0);
                    if &head[..n] == b"POST " {
                        let _ = rr.serve_http(s).await;
                    } else {
                        let _ = rr.serve(s).await;
                    }
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
            if fail.is_none() && role == "host" {
                // Deliveries that waited for the host, oldest first.
                for (id, sealed) in &room.hook_queue {
                    let _ = tx.send(Ctl::Msg(Message::Text(
                        json!({"type":"hook","delivery_id":id,"sealed":sealed}).to_string(),
                    )));
                }
            }
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
                // Answered in order with the control messages, echoing an id,
                // like the Worker.
                ("host", Message::Text(t))
                    if serde_json::from_str::<Value>(&t).is_ok_and(|v| v["type"] == "ping") =>
                {
                    let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                    let mut pong = json!({"type":"pong"});
                    if v["id"].is_u64() {
                        pong["id"] = v["id"].clone();
                    }
                    let _ = tx.send(Ctl::Msg(Message::Text(pong.to_string())));
                }
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
            Some("hooks.set") => {
                let key = v["seal_key"].as_str().and_then(|k| unb64(k).ok()).and_then(|k| <[u8; 32]>::try_from(k).ok());
                if let (Some(key), Some(hooks)) = (key, v["hooks"].as_array()) {
                    room.seal_key = Some(key);
                    room.hooks = hooks.iter().filter_map(|h| h.as_str().map(str::to_string)).collect();
                }
            }
            Some("hook.ack") => {
                if let Some(id) = v["delivery_id"].as_str() {
                    room.hook_queue.remove(id);
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
    /// `POST /v1/hook/{relay_id}/{hook_id}`: seal the request to the host,
    /// queue it, forward it if the host is connected. Returns the HTTP
    /// status and the delivery id.
    pub fn post_hook(&self, rid: &str, hook_id: &str, headers: &[(String, String)], body: &[u8]) -> (u16, Option<String>) {
        if body.len() > MAX_HOOK_BODY {
            return (413, None);
        }
        let mut st = self.state.lock().unwrap();
        let Some(room) = st.rooms.get_mut(rid) else { return (404, None) };
        let Some(key) = room.seal_key else { return (404, None) };
        if !room.hooks.contains(&b64(&agenthost_proto::tunnel::sha256(hook_id.as_bytes()))) {
            return (404, None);
        }
        if room.hook_queue.len() >= 200 {
            return (503, None);
        }
        let now = crate::util::now_ms();
        let id = format!("{now:013}-{}", crate::util::random_hex(8));
        let env = agenthost_proto::hookseal::Envelope {
            v: 1,
            hook_id: hook_id.to_string(),
            delivery_id: id.clone(),
            received_at: now,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
                .filter(|(k, _)| !k.starts_with("cf-") && !matches!(k.as_str(), "cookie" | "x-forwarded-for" | "x-real-ip" | "true-client-ip"))
                .collect(),
            body: b64(body),
        };
        let sealed = b64(&agenthost_proto::hookseal::seal(&key, &id, &serde_json::to_vec(&env).unwrap()));
        room.hook_queue.insert(id.clone(), sealed.clone());
        if let Some((_, h)) = &room.host {
            let _ = h.send(Ctl::Msg(Message::Text(json!({"type":"hook","delivery_id":id,"sealed":sealed}).to_string())));
        }
        (202, Some(id))
    }

    /// Deliveries the host hasn't acked yet.
    pub fn queued_hooks(&self, rid: &str) -> usize {
        self.state.lock().unwrap().rooms.get(rid).map_or(0, |r| r.hook_queue.len())
    }

    /// A minimal HTTP/1.1 server for webhook POSTs (`curl` against
    /// `agenthostd dev-relay`).
    async fn serve_http(&self, mut s: tokio::net::TcpStream) -> Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = Vec::new();
        let head_end = loop {
            let mut chunk = [0u8; 4096];
            let n = s.read(&mut chunk).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i;
            }
            if buf.len() > 16 * 1024 {
                return Ok(());
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let mut lines = head.split("\r\n");
        let path = lines.next().unwrap_or_default().split(' ').nth(1).unwrap_or_default().to_string();
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string())))
            .collect();
        let len: usize = headers.iter().find(|(k, _)| k == "content-length").and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
        let mut body = buf[head_end + 4..].to_vec();
        let status = if len > MAX_HOOK_BODY {
            (413, None)
        } else {
            while body.len() < len {
                let mut chunk = vec![0u8; len - body.len()];
                let n = s.read(&mut chunk).await?;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..n]);
            }
            let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
            match parts.as_slice() {
                ["v1", "hook", rid, hook] => self.post_hook(rid, hook, &headers, &body),
                _ => (404, None),
            }
        };
        let text = match status {
            (202, Some(id)) => json!({"delivery_id": id}).to_string(),
            (code, _) => json!({"error": code}).to_string(),
        };
        let resp = format!(
            "HTTP/1.1 {} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
            status.0,
            text.len()
        );
        s.write_all(resp.as_bytes()).await?;
        Ok(())
    }
}
