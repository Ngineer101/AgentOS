//! Push notifications to paired phones through Apple's APNs.
//!
//! Apple takes notifications for an app only with an APNs key of the Apple
//! Developer team that signs it. Usually that team's relay sends them: the
//! phone registers its APNs token with the relay its build names (`POST
//! /v1/push/register`) and gives the host only the sealed handle it gets
//! back; the host posts each notification to that relay, signed with its
//! relay key (`POST /v1/push/{relay_id}/send`), and needs no key of its own.
//!
//! A build signed by another team (built from source) has no such relay:
//! then the host talks to APNs itself (outbound HTTPS only; it still listens
//! on no port) with that team's APNs auth key, kept in the secret store. A
//! phone may hand it over while pairing (`push.set_key`), or `agenthost push
//! setup` does.
//!
//! Each phone registers its device token and a 32-byte key of its own. What a
//! notification says (session, tool, the agent's reply) is sealed with that
//! key (ChaCha20-Poly1305) and opened on the phone by the app's notification
//! service extension; Apple only carries the generic alert "A session needs
//! you".

use crate::bus::BusMsg;
use crate::ctx::Ctx;
use crate::secrets::SecretStore;
use crate::store::PushRow;
use crate::util::now_ms;
use agenthost_proto::jsonrpc::{codes, RpcError};
use agenthost_proto::tunnel::RelayKey;
use agenthost_proto::*;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

/// Secret-store name of the `.p8` key (PEM).
const KEY_SECRET: &str = "apns-auth-key";
/// Setting holding the key's id and the team id.
const KEY_SETTING: &str = "push.apns";
/// Authenticated data of every sealed notification.
pub const CONTENT_AAD: &[u8] = b"agenthost-push-v1";
/// What Apple (and a phone that can't open the content) sees.
pub const GENERIC_TITLE: &str = "AgentOS";
pub const GENERIC_BODY: &str = "A session needs you";
/// APNs wants a fresh provider token at least hourly and refuses one renewed
/// more often than every 20 minutes.
const TOKEN_TTL_MS: i64 = 50 * 60 * 1000;
/// How long APNs keeps trying an offline phone.
const EXPIRY: Duration = Duration::from_secs(60 * 60);
/// Longest body, in characters.
const BODY_MAX: usize = 180;
/// Longest relay URL and handle a phone may register.
const GATEWAY_URL_MAX: usize = 300;
const HANDLE_MAX: usize = 1024;

const PRODUCTION_URL: &str = "https://api.push.apple.com";
const SANDBOX_URL: &str = "https://api.sandbox.push.apple.com";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyInfo {
    key_id: String,
    team_id: String,
}

/// Who wants it (by preference), what it says, and an id that replaces an
/// earlier notification about the same thing.
type Notice = (fn(&PushPrefs) -> bool, PushContent, String);

struct Signer256 {
    key: SigningKey,
    info: KeyInfo,
}

pub struct Push {
    ctx: Arc<Ctx>,
    secrets: Arc<dyn SecretStore>,
    /// To APNs (HTTP/2).
    http: reqwest::Client,
    /// To relays that send for phones (`PushRow::gateway`).
    relay_http: reqwest::Client,
    /// The host's relay key, which signs what it asks a relay to send.
    relay_key: OnceLock<RelayKey>,
    /// The provider token in use and when it was made.
    token: Mutex<Option<(String, i64)>>,
}

/// Why APNs refused one notification.
#[derive(Debug, Clone)]
struct Refused {
    status: u16,
    reason: String,
}

fn bad(msg: impl Into<String>) -> RpcError {
    RpcError::invalid_params(msg.into())
}

fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::internal(e.to_string())
}

fn ten_chars(s: &str) -> bool {
    s.len() == 10 && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// Parse an APNs auth key (`AuthKey_XXXXXXXXXX.p8`): PEM PKCS#8, P-256.
pub fn parse_key(pem: &str) -> Result<SigningKey, RpcError> {
    let pem = pem.trim();
    if pem.is_empty() {
        return Err(bad("the APNs key is empty"));
    }
    SigningKey::from_pkcs8_pem(pem).or_else(|_| {
        // Some tools save it without the PEM armour.
        let b64: String = pem.chars().filter(|c| !c.is_whitespace()).collect();
        let der = STANDARD
            .decode(b64)
            .map_err(|_| bad("that is not an APNs key (.p8 file): expected a PEM private key"))?;
        SigningKey::from_pkcs8_der(&der)
            .map_err(|_| bad("that is not an APNs key (.p8 file): expected a P-256 private key"))
    })
}

/// A provider token: an ES256 JWT with the key id and the team id.
pub fn provider_token(key: &SigningKey, key_id: &str, team_id: &str, iat_secs: i64) -> String {
    let header = URL_SAFE_NO_PAD.encode(json!({"alg": "ES256", "kid": key_id}).to_string());
    let claims = URL_SAFE_NO_PAD.encode(json!({"iss": team_id, "iat": iat_secs}).to_string());
    let input = format!("{header}.{claims}");
    let sig: Signature = key.sign(input.as_bytes());
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
}

/// Seal a notification's content for one phone: nonce || ciphertext || tag,
/// as CryptoKit's `ChaChaPoly.SealedBox(combined:)` reads it.
pub fn seal(key: &[u8], content: &PushContent) -> anyhow::Result<String> {
    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| anyhow::anyhow!("a push key is 32 bytes"))?;
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let plain = serde_json::to_vec(content)?;
    let sealed = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: &plain, aad: CONTENT_AAD })
        .map_err(|_| anyhow::anyhow!("sealing failed"))?;
    let mut out = nonce.to_vec();
    out.extend(sealed);
    Ok(URL_SAFE_NO_PAD.encode(out))
}

/// Open what [`seal`] made (tests, and to document the format).
pub fn open(key: &[u8], sealed: &str) -> anyhow::Result<PushContent> {
    let raw = URL_SAFE_NO_PAD.decode(sealed)?;
    anyhow::ensure!(raw.len() > 12 + 16, "too short");
    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| anyhow::anyhow!("a push key is 32 bytes"))?;
    let plain = cipher
        .decrypt(Nonce::from_slice(&raw[..12]), Payload { msg: &raw[12..], aad: CONTENT_AAD })
        .map_err(|_| anyhow::anyhow!("does not open with this key"))?;
    Ok(serde_json::from_slice(&plain)?)
}

/// The APNs request body for one phone.
pub fn payload(key: &[u8], content: &PushContent) -> anyhow::Result<Value> {
    Ok(json!({
        "aps": {
            "alert": {"title": GENERIC_TITLE, "body": GENERIC_BODY},
            "sound": "default",
            "mutable-content": 1
        },
        "v": 1,
        "e": seal(key, content)?
    }))
}

fn shorten(s: &str, max: usize) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        return one_line;
    }
    let mut t: String = one_line.chars().take(max - 1).collect();
    t = t.trim_end().to_string();
    t.push('…');
    t
}

impl Push {
    pub fn new(ctx: Arc<Ctx>, secrets: Arc<dyn SecretStore>) -> Arc<Self> {
        let mut b = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .pool_idle_timeout(Duration::from_secs(10 * 60));
        if ctx
            .cfg
            .apns_url
            .as_deref()
            .is_some_and(|u| u.starts_with("http://"))
        {
            // A local test server: HTTP/2 without TLS.
            b = b.http2_prior_knowledge();
        }
        let relay_http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .expect("http client");
        Arc::new(Self {
            ctx,
            secrets,
            http: b.build().expect("http client"),
            relay_http,
            relay_key: OnceLock::new(),
            token: Mutex::new(None),
        })
    }

    /// The host's relay key (from remote access, once it has loaded it).
    pub fn set_relay_key(&self, key: RelayKey) {
        let _ = self.relay_key.set(key);
    }

    fn key_info(&self) -> Option<KeyInfo> {
        self.ctx.store.setting_get(KEY_SETTING).ok().flatten()
    }

    fn signer(&self) -> Option<Signer256> {
        let info = self.key_info()?;
        let pem = self.secrets.get(KEY_SECRET).ok().flatten()?;
        let key = parse_key(&String::from_utf8_lossy(&pem)).ok()?;
        Some(Signer256 { key, info })
    }

    pub fn status(&self, device_id: Option<&str>) -> Result<PushStatus, RpcError> {
        let info = self.key_info();
        let configured = info.is_some()
            && self.secrets.get(KEY_SECRET).ok().flatten().is_some();
        let (this_device, devices) = match device_id {
            Some(d) => (self.ctx.store.push_get(d).map_err(internal)?.map(|r| r.view()), vec![]),
            None => (
                None,
                self.ctx
                    .store
                    .push_list()
                    .map_err(internal)?
                    .iter()
                    .map(|r| r.view())
                    .collect(),
            ),
        };
        Ok(PushStatus {
            configured,
            key_id: info.as_ref().map(|i| i.key_id.clone()),
            team_id: info.map(|i| i.team_id),
            this_device,
            devices,
        })
    }

    pub fn set_key(&self, p: PushSetKeyParams, who: &str) -> Result<(), RpcError> {
        let key_id = p.key_id.trim().to_ascii_uppercase();
        let team_id = p.team_id.trim().to_ascii_uppercase();
        if !ten_chars(&key_id) {
            return Err(bad("the key id is the 10 letters and digits in the file name (AuthKey_<KEY ID>.p8)"));
        }
        if !ten_chars(&team_id) {
            return Err(bad("the team id is 10 letters and digits (Apple Developer → Membership)"));
        }
        if p.key_p8.len() > 4096 {
            return Err(bad("that file is too large to be an APNs key"));
        }
        parse_key(&p.key_p8)?;
        self.secrets
            .set(KEY_SECRET, p.key_p8.trim().as_bytes())
            .map_err(internal)?;
        self.ctx
            .store
            .setting_set(KEY_SETTING, &KeyInfo { key_id: key_id.clone(), team_id: team_id.clone() })
            .map_err(internal)?;
        *self.token.lock().unwrap() = None;
        self.ctx.audit.log(
            "push.key_set",
            json!({"key_id": key_id, "team_id": team_id, "by": who}),
        );
        Ok(())
    }

    pub fn clear_key(&self) -> Result<(), RpcError> {
        self.secrets.delete(KEY_SECRET).map_err(internal)?;
        self.ctx
            .store
            .setting_set(KEY_SETTING, &Value::Null)
            .map_err(internal)?;
        *self.token.lock().unwrap() = None;
        self.ctx.audit.log("push.key_cleared", json!({}));
        Ok(())
    }

    pub fn register(&self, device_id: &str, p: PushRegisterParams) -> Result<(), RpcError> {
        let gateway = p.gateway.as_ref().map(check_gateway).transpose()?;
        let token = p.token.trim().to_ascii_lowercase();
        if gateway.is_some() {
            if !token.is_empty() {
                return Err(bad("give the device token or a relay's handle, not both"));
            }
        } else if token.len() < 32 || token.len() > 400 || token.len() & 1 == 1 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(bad("the device token must be hex"));
        }
        let topic = p.topic.trim();
        if topic.is_empty()
            || topic.len() > 200
            || !topic.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        {
            return Err(bad("the topic is the app's bundle id"));
        }
        let key = URL_SAFE_NO_PAD
            .decode(p.key.trim())
            .map_err(|_| bad("the push key must be base64url"))?;
        if key.len() != 32 {
            return Err(bad("the push key must be 32 bytes"));
        }
        self.ctx
            .store
            .push_register(device_id, &token, gateway.as_ref(), topic, p.environment, &key, &p.prefs)
            .map_err(internal)
    }

    pub fn unregister(&self, device_id: &str) -> Result<(), RpcError> {
        self.ctx.store.push_unregister(device_id).map_err(internal)?;
        Ok(())
    }

    pub fn set_prefs(&self, device_id: &str, prefs: &PushPrefs) -> Result<(), RpcError> {
        if !self.ctx.store.push_set_prefs(device_id, prefs).map_err(internal)? {
            return Err(RpcError::not_found("this device is not registered for notifications"));
        }
        Ok(())
    }

    /// Send a test notification to one device, or to every registered one.
    pub async fn test(&self, device_id: Option<&str>) -> Result<PushTestResult, RpcError> {
        let signer = self.signer();
        let rows: Vec<PushRow> = match device_id {
            Some(d) => vec![self
                .ctx
                .store
                .push_get(d)
                .map_err(internal)?
                .ok_or_else(|| RpcError::not_found("this device is not registered for notifications"))?],
            None => self.ctx.store.push_list().map_err(internal)?,
        };
        if rows.is_empty() {
            return Err(RpcError::not_found("no device is registered for notifications"));
        }
        if signer.is_none() && rows.iter().all(|r| r.gateway.is_none()) {
            return Err(RpcError::new(codes::UNAVAILABLE, "the host has no APNs key yet"));
        }
        let content = PushContent {
            kind: "test".into(),
            title: "Notifications are on".into(),
            body: "You'll hear from AgentOS when a session needs you.".into(),
            session_id: None,
            approval_id: None,
        };
        let mut out = PushTestResult { sent: 0, errors: vec![] };
        for row in rows {
            match self.deliver(signer.as_ref(), &row, &content, None).await {
                Ok(()) => out.sent += 1,
                Err(e) => out.errors.push(format!(
                    "{}: {}",
                    row.name,
                    push_error_text(&e.reason, row.gateway.is_some())
                )),
            }
        }
        Ok(out)
    }

    /// Watch the bus and notify registered phones.
    pub fn spawn(self: &Arc<Self>) {
        let me = self.clone();
        let mut rx = self.ctx.bus.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(BusMsg::Event(ev)) => {
                        if let Some((pref, content, collapse)) = me.content_for(&ev) {
                            // APNs may be slow; never hold up the bus.
                            let me = me.clone();
                            tokio::spawn(async move { me.notify(pref, content, collapse).await });
                        }
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!("push: missed {n} host messages; some notifications may not be sent")
                    }
                    Err(RecvError::Closed) => return,
                }
            }
        });
    }

    fn session_label(&self, s: &Session) -> String {
        if let Some(t) = s.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            return shorten(t, 80);
        }
        if s.workspace_id == QUICK_WORKSPACE_ID {
            return "Quick task".into();
        }
        self.ctx
            .store
            .workspace_get(&s.workspace_id)
            .ok()
            .flatten()
            .map(|w| w.name)
            .unwrap_or_else(|| "Session".into())
    }

    /// What to say about an event, if anything.
    fn content_for(&self, ev: &Event) -> Option<Notice> {
        let kind = ev.kind;
        if !matches!(
            kind,
            EventType::ApprovalRequested | EventType::TurnEnd | EventType::SessionState
        ) {
            return None;
        }
        if self.ctx.store.push_list().ok()?.is_empty() {
            return None;
        }
        let s = self.ctx.store.session_get(&ev.session_id).ok().flatten()?;
        let agent = s.agent.display_name();
        let label = self.session_label(&s);
        match kind {
            EventType::ApprovalRequested => {
                let a: PendingApproval = serde_json::from_value(ev.payload.clone()).ok()?;
                let what = a
                    .summary
                    .clone()
                    .unwrap_or_else(|| format!("{} · {}", a.connector_name, a.tool));
                Some((
                    |p: &PushPrefs| p.approvals,
                    PushContent {
                        kind: "approval".into(),
                        title: label,
                        body: shorten(&format!("{agent} needs your approval: {what}"), BODY_MAX),
                        session_id: Some(s.session_id.clone()),
                        approval_id: Some(a.approval_id.clone()),
                    },
                    a.approval_id,
                ))
            }
            EventType::TurnEnd if self.automation_turn(&s, ev).is_some() => {
                let name = self.automation_turn(&s, ev)?;
                let stop = ev.payload["stop_reason"].as_str().unwrap_or("end_turn");
                let body = match stop {
                    "cancelled" => return None,
                    "error" => format!("{agent} stopped with an error"),
                    _ => {
                        let prompt_seq = ev.payload["prompt_seq"].as_i64().unwrap_or(0);
                        match self.last_reply(&s.session_id, prompt_seq, ev.seq) {
                            Some(t) => format!("Finished: {t}"),
                            None => "Finished".to_string(),
                        }
                    }
                };
                Some((
                    |p: &PushPrefs| p.automations,
                    PushContent {
                        kind: "automation".into(),
                        title: name,
                        body: shorten(&body, BODY_MAX),
                        session_id: Some(s.session_id.clone()),
                        approval_id: None,
                    },
                    format!("automation-{}", s.session_id),
                ))
            }
            EventType::TurnEnd => {
                let stop = ev.payload["stop_reason"].as_str().unwrap_or("end_turn");
                if stop == "cancelled" {
                    return None;
                }
                let body = if stop == "error" {
                    format!("{agent} stopped with an error")
                } else {
                    let prompt_seq = ev.payload["prompt_seq"].as_i64().unwrap_or(0);
                    match self.last_reply(&s.session_id, prompt_seq, ev.seq) {
                        Some(t) => format!("{agent}: {t}"),
                        None => format!("{agent} finished"),
                    }
                };
                Some((
                    |p: &PushPrefs| p.turn_done,
                    PushContent {
                        kind: "turn_done".into(),
                        title: label,
                        body: shorten(&body, BODY_MAX),
                        session_id: Some(s.session_id.clone()),
                        approval_id: None,
                    },
                    format!("turn-{}", s.session_id),
                ))
            }
            EventType::SessionState => {
                if ev.payload["state"].as_str() != Some(SessionState::Failed.as_str()) {
                    return None;
                }
                let reason = ev.payload["reason"].as_str().unwrap_or("the session stopped");
                if let Some(a) = s.automation_id.as_deref().and_then(|id| self.ctx.store.automation_get(id).ok().flatten()) {
                    return Some((
                        |p: &PushPrefs| p.automations,
                        PushContent {
                            kind: "automation".into(),
                            title: shorten(&a.automation.name, 80),
                            body: shorten(&format!("{agent} failed: {reason}"), BODY_MAX),
                            session_id: Some(s.session_id.clone()),
                            approval_id: None,
                        },
                        format!("failed-{}", s.session_id),
                    ));
                }
                Some((
                    |p: &PushPrefs| p.failed,
                    PushContent {
                        kind: "failed".into(),
                        title: label,
                        body: shorten(&format!("{agent} failed: {reason}"), BODY_MAX),
                        session_id: Some(s.session_id.clone()),
                        approval_id: None,
                    },
                    format!("failed-{}", s.session_id),
                ))
            }
            _ => None,
        }
    }

    /// The automation's name, when this turn answers the prompt an
    /// automation's run sent (not a follow-up you asked in its session).
    fn automation_turn(&self, s: &Session, ev: &Event) -> Option<String> {
        let id = s.automation_id.as_deref()?;
        let prompt_seq = ev.payload["prompt_seq"].as_i64()?;
        let prompt = self.ctx.store.events_after(&s.session_id, prompt_seq - 1, 1).ok()?.into_iter().next()?;
        if prompt.payload["device"].as_str() != Some("automation") {
            return None;
        }
        let a = self.ctx.store.automation_get(id).ok()??;
        Some(shorten(&a.automation.name, 80))
    }

    /// The agent's last reply in a turn.
    fn last_reply(&self, session_id: &str, after_seq: i64, before_seq: i64) -> Option<String> {
        let evs = self
            .ctx
            .store
            .events_after(session_id, after_seq, 2000)
            .ok()?;
        evs.iter()
            .rev()
            .filter(|e| e.seq < before_seq && e.kind == EventType::AgentMessage)
            .find_map(|e| e.payload["text"].as_str().map(str::trim).filter(|t| !t.is_empty()).map(str::to_string))
    }

    async fn notify(&self, wants: fn(&PushPrefs) -> bool, content: PushContent, collapse: String) {
        let rows: Vec<PushRow> = match self.ctx.store.push_list() {
            Ok(r) => r.into_iter().filter(|r| wants(&r.prefs)).collect(),
            Err(e) => {
                tracing::warn!("push: {e}");
                return;
            }
        };
        if rows.is_empty() {
            return;
        }
        let signer = self.signer();
        for row in rows {
            if let Err(e) = self.deliver(signer.as_ref(), &row, &content, Some(&collapse)).await {
                tracing::warn!(device = %row.device_id, status = e.status, reason = %e.reason, "push refused");
            }
        }
    }

    fn provider_token(&self, s: &Signer256, fresh: bool) -> String {
        let mut t = self.token.lock().unwrap();
        let now = now_ms();
        match &*t {
            Some((tok, at)) if !fresh && now - at < TOKEN_TTL_MS => tok.clone(),
            _ => {
                let tok = provider_token(&s.key, &s.info.key_id, &s.info.team_id, now / 1000);
                *t = Some((tok.clone(), now));
                tok
            }
        }
    }

    fn base_url(&self, env: PushEnvironment) -> String {
        if let Some(u) = &self.ctx.cfg.apns_url {
            return format!("{}/{}", u.trim_end_matches('/'), env.as_str());
        }
        match env {
            PushEnvironment::Production => PRODUCTION_URL.into(),
            PushEnvironment::Sandbox => SANDBOX_URL.into(),
        }
    }

    async fn post(
        &self,
        s: &Signer256,
        row: &PushRow,
        env: PushEnvironment,
        body: &Value,
        collapse: Option<&str>,
        fresh_token: bool,
    ) -> Result<(), Refused> {
        let url = format!("{}/3/device/{}", self.base_url(env), row.token);
        let expires = (now_ms() / 1000) + EXPIRY.as_secs() as i64;
        let mut req = self
            .http
            .post(url)
            .bearer_auth(self.provider_token(s, fresh_token))
            .header("apns-topic", &row.topic)
            .header("apns-push-type", "alert")
            .header("apns-priority", "10")
            .header("apns-expiration", expires.to_string())
            .json(body);
        if let Some(c) = collapse {
            req = req.header("apns-collapse-id", c);
        }
        let resp = req.send().await.map_err(|e| Refused {
            status: 0,
            reason: format!("could not reach APNs: {e}"),
        })?;
        let status = resp.status().as_u16();
        if status == 200 {
            return Ok(());
        }
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        Err(Refused {
            status,
            reason: v["reason"].as_str().unwrap_or("unknown").to_string(),
        })
    }

    /// Ask the phone's relay to send it (`POST {url}/v1/push/{relay_id}/send`).
    /// The relay answers as APNs would; it switches environments itself.
    async fn post_gateway(
        &self,
        g: &PushGateway,
        row: &PushRow,
        content: &PushContent,
        collapse: Option<&str>,
    ) -> Result<Option<PushEnvironment>, Refused> {
        let key = self.relay_key.get().ok_or_else(|| Refused {
            status: 0,
            reason: "remote access hasn't started yet".into(),
        })?;
        let sealed = seal(&row.content_key, content).map_err(|e| Refused {
            status: 0,
            reason: e.to_string(),
        })?;
        let mut msg = json!({"handle": g.handle, "sealed": sealed});
        if let Some(c) = collapse {
            msg["collapse_id"] = json!(c);
        }
        let body = msg.to_string().into_bytes();
        let time = now_ms();
        let url = format!(
            "{}/v1/push/{}/send",
            g.url.trim_end_matches('/'),
            key.relay_id()
        );
        let resp = self
            .relay_http
            .post(url)
            .header("content-type", "application/json")
            .header("x-agenthost-key", agenthost_proto::tunnel::b64(&key.public()))
            .header("x-agenthost-time", time.to_string())
            .header("x-agenthost-signature", key.sign_push(time, &body))
            .body(body)
            .send()
            .await
            .map_err(|e| Refused {
                status: 0,
                reason: format!("could not reach the relay that sends notifications: {e}"),
            })?;
        let status = resp.status().as_u16();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        if status == 200 {
            return Ok(v["environment"].as_str().and_then(PushEnvironment::parse));
        }
        Err(Refused {
            status,
            reason: v["reason"].as_str().unwrap_or("unknown").to_string(),
        })
    }

    async fn deliver(
        &self,
        s: Option<&Signer256>,
        row: &PushRow,
        content: &PushContent,
        collapse: Option<&str>,
    ) -> Result<(), Refused> {
        let r = match (&row.gateway, s) {
            (Some(g), _) => match self.post_gateway(g, row, content, collapse).await {
                Ok(env) => {
                    if let Some(env) = env.filter(|e| *e != row.environment) {
                        let _ = self.ctx.store.push_set_environment(&row.device_id, env);
                    }
                    Ok(())
                }
                Err(e) => Err(e),
            },
            (None, Some(s)) => self.deliver_direct(s, row, content, collapse).await,
            (None, None) => {
                tracing::warn!(device = %row.device_id, "push: the host has no APNs key; not notifying");
                Err(Refused { status: 0, reason: "NoAPNsKey".into() })
            }
        };
        match &r {
            Ok(()) => {
                if row.last_error.is_some() {
                    let _ = self.ctx.store.push_set_error(&row.device_id, None);
                }
            }
            // The app was deleted or notifications turned off for it.
            Err(e) if e.status == 410 || e.reason == "Unregistered" => {
                let _ = self.ctx.store.push_unregister(&row.device_id);
                self.ctx.audit.log(
                    "push.unregistered",
                    json!({"device_id": row.device_id, "reason": e.reason}),
                );
            }
            Err(e) => {
                let _ = self.ctx.store.push_set_error(&row.device_id, Some(&e.reason));
            }
        }
        tracing::debug!(device = %row.device_id, relay = row.gateway.is_some(), ok = r.is_ok(), "push sent");
        r
    }

    /// Straight to APNs with the host's own key.
    async fn deliver_direct(
        &self,
        s: &Signer256,
        row: &PushRow,
        content: &PushContent,
        collapse: Option<&str>,
    ) -> Result<(), Refused> {
        let body = payload(&row.content_key, content).map_err(|e| Refused {
            status: 0,
            reason: e.to_string(),
        })?;
        let env = row.environment;
        let mut r = self.post(s, row, env, &body, collapse, false).await;
        if let Err(e) = &r {
            match e.reason.as_str() {
                // The provider token aged out or the key changed: make a new one.
                "ExpiredProviderToken" | "InvalidProviderToken" => {
                    r = self.post(s, row, env, &body, collapse, true).await;
                }
                // A token from the other environment (a debug build that
                // thought it was a release, or the other way round).
                "BadDeviceToken" => {
                    let other = env.other();
                    if self.post(s, row, other, &body, collapse, false).await.is_ok() {
                        let _ = self.ctx.store.push_set_environment(&row.device_id, other);
                        r = Ok(());
                    }
                }
                _ => {}
            }
        }
        r
    }
}

/// A relay a phone named for its notifications: `https://` (plain `http://`
/// only to this machine, for the dev relay), no user or password, and a
/// base64url handle.
fn check_gateway(g: &PushGateway) -> Result<PushGateway, RpcError> {
    let url = g.url.trim().trim_end_matches('/');
    let parsed = reqwest::Url::parse(url).map_err(|_| bad("the relay's URL is not a URL"))?;
    let local = matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    let scheme_ok = parsed.scheme() == "https" || (parsed.scheme() == "http" && local);
    if url.len() > GATEWAY_URL_MAX
        || !scheme_ok
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(bad("the relay's URL must be https://<host>"));
    }
    let handle = g.handle.trim();
    if handle.is_empty()
        || handle.len() > HANDLE_MAX
        || !handle.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(bad("the relay's handle must be base64url"));
    }
    Ok(PushGateway { url: url.to_string(), handle: handle.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::VerifyingKey;
    use p256::pkcs8::{EncodePrivateKey, LineEnding};

    #[test]
    fn seal_opens_with_the_key_only() {
        let key = [7u8; 32];
        let c = PushContent {
            kind: "approval".into(),
            title: "relay".into(),
            body: "Claude Code needs your approval: Push agenthost/x".into(),
            session_id: Some("ses_1".into()),
            approval_id: Some("apr_1".into()),
        };
        let sealed = seal(&key, &c).unwrap();
        assert_eq!(open(&key, &sealed).unwrap(), c);
        assert!(open(&[8u8; 32], &sealed).is_err());
        let p = payload(&key, &c).unwrap();
        assert_eq!(p["aps"]["alert"]["body"], GENERIC_BODY);
        assert!(!p.to_string().contains("relay"));
    }

    #[test]
    fn provider_token_verifies() {
        let sk = SigningKey::random(&mut rand::rngs::OsRng);
        let pem = sk.to_pkcs8_pem(LineEnding::LF).unwrap();
        let parsed = parse_key(&pem).unwrap();
        let tok = provider_token(&parsed, "ABC123DEFG", "TEAM123456", 1_700_000_000);
        let parts: Vec<&str> = tok.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header, json!({"alg": "ES256", "kid": "ABC123DEFG"}));
        let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims, json!({"iss": "TEAM123456", "iat": 1_700_000_000}));
        let sig = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
        VerifyingKey::from(&sk)
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
            .unwrap();
    }

    #[test]
    fn rejects_what_is_not_a_key() {
        assert!(parse_key("").is_err());
        assert!(parse_key("-----BEGIN PRIVATE KEY-----\nnope\n-----END PRIVATE KEY-----").is_err());
        let sk = SigningKey::random(&mut rand::rngs::OsRng);
        let pem = sk.to_pkcs8_pem(LineEnding::LF).unwrap();
        let bare: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
        assert!(parse_key(&bare).is_ok());
    }

    #[test]
    fn shortens_on_one_line() {
        assert_eq!(shorten("a\n b", 10), "a b");
        assert_eq!(shorten("abcdefghijk", 5), "abcd…");
    }

    /// The notifier against an APNs stand-in (HTTP/2 without TLS), with a
    /// real host's store but none of its sockets.
    mod delivery {
        use super::*;
        use crate::store::DeviceRecord;
        use agenthost_proto::methods::Capability;
        use http_body_util::{BodyExt, Full};
        use hyper::body::Bytes;

        type Seen = Arc<Mutex<Vec<(String, Option<String>, Value)>>>;

        /// Answers 410 for tokens starting `dead`, `BadDeviceToken` outside
        /// the sandbox for tokens starting `5b`, else 200.
        async fn fake_apns() -> (String, Seen) {
            use hyper::server::conn::http2;
            use hyper::service::service_fn;
            use hyper_util::rt::{TokioExecutor, TokioIo};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let seen: Seen = Arc::default();
            let s = seen.clone();
            tokio::spawn(async move {
                while let Ok((tcp, _)) = listener.accept().await {
                    let s = s.clone();
                    tokio::spawn(async move {
                        let svc = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                            let s = s.clone();
                            async move {
                                assert_eq!(req.version(), hyper::Version::HTTP_2);
                                let path = req.uri().path().to_string();
                                let collapse = req.headers().get("apns-collapse-id").map(|v| v.to_str().unwrap().to_string());
                                assert!(req.headers()["authorization"].to_str().unwrap().starts_with("Bearer "));
                                let body = req.into_body().collect().await.unwrap().to_bytes();
                                let token = path.rsplit('/').next().unwrap().to_string();
                                let (status, reason) = if token.starts_with("dead") {
                                    (410, "Unregistered")
                                } else if token.starts_with("5b") && !path.starts_with("/sandbox/") {
                                    (400, "BadDeviceToken")
                                } else {
                                    s.lock().unwrap().push((path, collapse, serde_json::from_slice(&body).unwrap()));
                                    (200, "")
                                };
                                let body = if reason.is_empty() { Bytes::new() } else { Bytes::from(json!({"reason": reason}).to_string()) };
                                Ok::<_, std::convert::Infallible>(hyper::Response::builder().status(status).body(Full::new(body)).unwrap())
                            }
                        });
                        let _ = http2::Builder::new(TokioExecutor::new()).serve_connection(TokioIo::new(tcp), svc).await;
                    });
                }
            });
            (url, seen)
        }

        async fn wait(seen: &Seen, n: usize) -> Vec<(String, Option<String>, Value)> {
            for _ in 0..200 {
                if seen.lock().unwrap().len() >= n {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let v = seen.lock().unwrap().clone();
            assert_eq!(v.len(), n, "{v:?}");
            v
        }

        #[tokio::test]
        async fn notifies_registered_phones_about_what_they_asked_for() {
            let dir = tempfile::tempdir().unwrap();
            let (url, seen) = fake_apns().await;
            let mut cfg = crate::HostConfig::in_dir(dir.path());
            cfg.apns_url = Some(url);
            let host = crate::Host::open(cfg).unwrap();
            let ctx = host.ctx.clone();
            let push = host.push.clone();
            push.spawn();

            let phone = |id: &str| DeviceRecord {
                device: Device {
                    device_id: id.into(),
                    name: format!("Phone {id}"),
                    capabilities: Capability::phone(),
                    created_at: 0,
                    last_seen_at: None,
                    revoked_at: None,
                },
                public_key: format!("pk-{id}"),
                relay_key: format!("rk-{id}"),
            };
            ctx.store.device_insert(&phone("dev_a")).unwrap();
            ctx.store.device_insert(&phone("dev_b")).unwrap();
            let sk = SigningKey::random(&mut rand::rngs::OsRng);
            push.set_key(
                PushSetKeyParams {
                    key_p8: sk.to_pkcs8_pem(LineEnding::LF).unwrap().to_string(),
                    key_id: "KEY1234567".into(),
                    team_id: "TEAM123456".into(),
                },
                "test",
            )
            .unwrap();
            let (ka, kb) = ([1u8; 32], [2u8; 32]);
            let reg = |token: &str, key: &[u8], prefs: PushPrefs| PushRegisterParams {
                token: token.into(),
                gateway: None,
                topic: "dev.agenthost.ios".into(),
                environment: PushEnvironment::Production,
                key: URL_SAFE_NO_PAD.encode(key),
                prefs,
            };
            let a_token = "a0".repeat(32);
            push.register("dev_a", reg(&a_token, &ka, PushPrefs::default())).unwrap();
            // B: a debug build that thinks it is a release; wants everything.
            let all = PushPrefs { approvals: true, turn_done: true, failed: true, automations: true };
            push.register("dev_b", reg(&"5b".repeat(32), &kb, all)).unwrap();

            let sid = "ses_1";
            ctx.store
                .session_insert(&Session {
                    session_id: sid.into(),
                    workspace_id: QUICK_WORKSPACE_ID.into(),
                    agent: AgentKind::Claude,
                    profile_id: None,
                    archived_at: None,
                    worktree: None,
                    config_options: vec![],
                    side_questions: false,
                    pull_request: None,
                    pinned_at: None,
                    automation_id: None,
                    state: SessionState::Running,
                    state_reason: None,
                    acp_session_id: None,
                    title: None,
                    created_at: 0,
                    updated_at: 0,
                    last_seq: 0,
                })
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;

            // An approval: both phones.
            let approval = PendingApproval {
                approval_id: "apr_1".into(),
                session_id: sid.into(),
                connector_id: "con_1".into(),
                connector_name: "GitHub".into(),
                tool: "create_issue".into(),
                arguments: json!({}),
                requested_at: 0,
                expires_at: 0,
                summary: None,
            };
            ctx.append(sid, EventType::ApprovalRequested, serde_json::to_value(&approval).unwrap()).unwrap();
            let v = wait(&seen, 2).await;
            let (a, b): (Vec<_>, Vec<_>) = v.iter().partition(|x| x.0.ends_with(&a_token));
            assert_eq!(a[0].0, format!("/production/3/device/{a_token}"));
            assert!(b[0].0.starts_with("/sandbox/"), "B's token works in the sandbox only");
            assert_eq!(a[0].1.as_deref(), Some("apr_1"));
            let c = open(&ka, a[0].2["e"].as_str().unwrap()).unwrap();
            assert_eq!(c.title, "Quick task");
            assert_eq!(c.body, "Claude Code needs your approval: GitHub · create_issue");
            assert_eq!(open(&kb, b[0].2["e"].as_str().unwrap()).unwrap(), c);
            assert!(open(&kb, a[0].2["e"].as_str().unwrap()).is_err(), "sealed to each phone");
            assert_eq!(ctx.store.push_get("dev_b").unwrap().unwrap().environment, PushEnvironment::Sandbox);

            // A finished turn, with the reply: only B asked. A cancelled one: nobody.
            let p = ctx.store.append_event(sid, EventType::Prompt, json!({"text": "hi"})).unwrap();
            ctx.append(sid, EventType::AgentMessage, json!({"text": "Let me look."})).unwrap();
            ctx.append(sid, EventType::AgentMessage, json!({"text": "Done:\nthe tests pass."})).unwrap();
            ctx.append(sid, EventType::TurnEnd, json!({"stop_reason": "cancelled", "prompt_seq": p.seq})).unwrap();
            ctx.append(sid, EventType::TurnEnd, json!({"stop_reason": "end_turn", "prompt_seq": p.seq})).unwrap();
            let v = wait(&seen, 3).await;
            assert!(v[2].0.starts_with("/sandbox/"));
            assert_eq!(v[2].1.as_deref(), Some("turn-ses_1"));
            let c = open(&kb, v[2].2["e"].as_str().unwrap()).unwrap();
            assert_eq!((c.kind.as_str(), c.body.as_str()), ("turn_done", "Claude Code: Done: the tests pass."));

            // Failed: B again. Then A's app is deleted: its registration goes.
            ctx.set_state(sid, SessionState::Failed, Some("not logged in"), None).unwrap();
            let v = wait(&seen, 4).await;
            let c = open(&kb, v[3].2["e"].as_str().unwrap()).unwrap();
            assert_eq!((c.kind.as_str(), c.body.as_str()), ("failed", "Claude Code failed: not logged in"));
            push.register("dev_a", reg(&"dead".repeat(16), &ka, PushPrefs::default())).unwrap();
            let r = push.test(Some("dev_a")).await.unwrap();
            assert_eq!(r.sent, 0);
            assert!(r.errors[0].starts_with("Phone dev_a: The app was removed") && r.errors[0].contains("(Unregistered)"), "{:?}", r.errors);
            assert!(ctx.store.push_get("dev_a").unwrap().is_none());

            // Revoked phones get nothing.
            ctx.store.device_revoke("dev_b").unwrap();
            assert!(push.status(None).unwrap().devices.is_empty());
            ctx.append(sid, EventType::ApprovalRequested, serde_json::to_value(&approval).unwrap()).unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert_eq!(seen.lock().unwrap().len(), 4);

            push.clear_key().unwrap();
            let st = push.status(None).unwrap();
            assert!(!st.configured && st.key_id.is_none());
            assert!(push.test(None).await.is_err());
        }

        type Sent = Arc<Mutex<Vec<Value>>>;

        /// A relay's push service: checks the host's signature, answers 410
        /// for the handle `dead`, says a handle `h-sandbox` worked in the
        /// sandbox, and keeps what it was asked to send.
        async fn fake_relay(relay_id: String) -> (String, Sent) {
            use hyper::server::conn::http1;
            use hyper::service::service_fn;
            use hyper_util::rt::TokioIo;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let sent: Sent = Arc::default();
            let s = sent.clone();
            tokio::spawn(async move {
                while let Ok((tcp, _)) = listener.accept().await {
                    let (s, rid) = (s.clone(), relay_id.clone());
                    tokio::spawn(async move {
                        let svc = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                            let (s, rid) = (s.clone(), rid.clone());
                            async move {
                                assert_eq!(req.uri().path(), format!("/v1/push/{rid}/send"));
                                let h = |n: &str| req.headers()[n].to_str().unwrap().to_string();
                                let (key, time, sig) = (h("x-agenthost-key"), h("x-agenthost-time"), h("x-agenthost-signature"));
                                let body = req.into_body().collect().await.unwrap().to_bytes();
                                let signed = agenthost_proto::tunnel::verify_push(
                                    &agenthost_proto::tunnel::unb64(&key).unwrap(),
                                    &rid,
                                    time.parse().unwrap(),
                                    &body,
                                    &agenthost_proto::tunnel::unb64(&sig).unwrap(),
                                );
                                let msg: Value = serde_json::from_slice(&body).unwrap();
                                let (status, answer) = if !signed {
                                    (401, json!({"reason": "BadSignature"}))
                                } else if msg["handle"] == "dead" {
                                    (410, json!({"reason": "Unregistered"}))
                                } else {
                                    s.lock().unwrap().push(msg.clone());
                                    let env = if msg["handle"] == "h-sandbox" { "sandbox" } else { "production" };
                                    (200, json!({"environment": env}))
                                };
                                Ok::<_, std::convert::Infallible>(
                                    hyper::Response::builder()
                                        .status(status)
                                        .body(Full::new(Bytes::from(answer.to_string())))
                                        .unwrap(),
                                )
                            }
                        });
                        let _ = http1::Builder::new().serve_connection(TokioIo::new(tcp), svc).await;
                    });
                }
            });
            (url, sent)
        }

        #[tokio::test]
        async fn a_relay_sends_for_phones_so_the_host_needs_no_key() {
            let dir = tempfile::tempdir().unwrap();
            let host = crate::Host::open(crate::HostConfig::in_dir(dir.path())).unwrap();
            let ctx = host.ctx.clone();
            let push = host.push.clone();
            let relay_key = RelayKey::generate();
            let (url, sent) = fake_relay(relay_key.relay_id()).await;
            push.set_relay_key(relay_key);
            push.spawn();
            for id in ["dev_a", "dev_b", "dev_c"] {
                ctx.store
                    .device_insert(&DeviceRecord {
                        device: Device {
                            device_id: id.into(),
                            name: format!("Phone {id}"),
                            capabilities: Capability::phone(),
                            created_at: 0,
                            last_seen_at: None,
                            revoked_at: None,
                        },
                        public_key: format!("pk-{id}"),
                        relay_key: format!("rk-{id}"),
                    })
                    .unwrap();
            }
            let ka = [3u8; 32];
            let via = |handle: &str, key: &[u8]| PushRegisterParams {
                token: String::new(),
                gateway: Some(PushGateway { url: format!("{url}/"), handle: handle.into() }),
                topic: "dev.agenthost.ios".into(),
                environment: PushEnvironment::Production,
                key: URL_SAFE_NO_PAD.encode(key),
                prefs: PushPrefs::default(),
            };

            // What a phone may name.
            for (u, h) in [("http://relay.example", "h"), ("https://u:p@relay.example", "h"), ("https://relay.example", "not base64!")] {
                let mut p = via("h", &ka);
                p.gateway = Some(PushGateway { url: u.into(), handle: h.into() });
                assert!(push.register("dev_a", p).is_err(), "{u} {h}");
            }
            let mut both = via("h", &ka);
            both.token = "a0".repeat(32);
            assert!(push.register("dev_a", both).is_err());

            push.register("dev_a", via("h-a", &ka)).unwrap();
            let st = push.status(Some("dev_a")).unwrap();
            assert!(!st.configured, "no APNs key on the host");
            assert_eq!(st.this_device.unwrap().gateway.as_deref(), Some(url.as_str()));

            // An approval: through the relay, sealed to the phone.
            ctx.store
                .session_insert(&Session {
                    session_id: "ses_1".into(),
                    workspace_id: QUICK_WORKSPACE_ID.into(),
                    agent: AgentKind::Claude,
                    profile_id: None,
                    archived_at: None,
                    worktree: None,
                    config_options: vec![],
                    side_questions: false,
                    pull_request: None,
                    pinned_at: None,
                    automation_id: None,
                    state: SessionState::Running,
                    state_reason: None,
                    acp_session_id: None,
                    title: None,
                    created_at: 0,
                    updated_at: 0,
                    last_seq: 0,
                })
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            let approval = PendingApproval {
                approval_id: "apr_1".into(),
                session_id: "ses_1".into(),
                connector_id: "con_1".into(),
                connector_name: "GitHub".into(),
                tool: "create_issue".into(),
                arguments: json!({}),
                requested_at: 0,
                expires_at: 0,
                summary: None,
            };
            ctx.append("ses_1", EventType::ApprovalRequested, serde_json::to_value(&approval).unwrap()).unwrap();
            for _ in 0..200 {
                if !sent.lock().unwrap().is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let got = sent.lock().unwrap().clone();
            assert_eq!(got.len(), 1, "{got:?}");
            assert_eq!((got[0]["handle"].as_str(), got[0]["collapse_id"].as_str()), (Some("h-a"), Some("apr_1")));
            let c = open(&ka, got[0]["sealed"].as_str().unwrap()).unwrap();
            assert_eq!(c.body, "Claude Code needs your approval: GitHub · create_issue");

            // A test works without a key; one that worked in the sandbox is remembered.
            push.register("dev_b", via("h-sandbox", &[4u8; 32])).unwrap();
            let r = push.test(Some("dev_b")).await.unwrap();
            assert_eq!((r.sent, r.errors.len()), (1, 0));
            assert_eq!(push.status(Some("dev_b")).unwrap().this_device.unwrap().environment, PushEnvironment::Sandbox);

            // A phone that registered its token needs the host's key; it says so.
            let mut direct = via("", &[5u8; 32]);
            direct.gateway = None;
            direct.token = "c0".repeat(32);
            push.register("dev_c", direct).unwrap();
            let r = push.test(None).await.unwrap();
            assert_eq!((r.sent, r.errors.len()), (2, 1));
            assert!(r.errors[0].starts_with("Phone dev_c: Nothing was sent") && r.errors[0].contains("(NoAPNsKey)"), "{:?}", r.errors);
            let dev_c = push.status(Some("dev_c")).unwrap().this_device.unwrap();
            assert!(dev_c.last_error_text.unwrap().contains("no relay sends this iPhone's notifications"));
            assert_eq!(push.status(Some("dev_c")).unwrap().this_device.unwrap().last_error.as_deref(), Some("NoAPNsKey"));
            assert!(push.test(Some("dev_c")).await.is_err(), "only key-less phones: no APNs key yet");

            // The app was deleted: the relay says Unregistered, the host forgets it.
            push.register("dev_a", via("dead", &ka)).unwrap();
            let r = push.test(Some("dev_a")).await.unwrap();
            assert_eq!(r.sent, 0);
            assert!(push.status(Some("dev_a")).unwrap().this_device.is_none());
        }
    }
}
