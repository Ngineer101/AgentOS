//! End-to-end tunnel between a paired device and the host, carried as opaque
//! binary frames by the relay. One implementation, shared by the host, the
//! command-line client and (through bindings) the iOS app.
//!
//! - Pairing: `Noise_XXpsk3_25519_ChaChaPoly_BLAKE2s`. The phone is the
//!   initiator; the PSK is the 32-byte secret from the QR code; the phone
//!   checks the host's static key against the QR code. The phone's third
//!   message carries its name and relay key. Both sides show a 6-digit code
//!   derived from the handshake hash.
//! - Later connections: `Noise_IK_25519_ChaChaPoly_BLAKE2s`, phone initiates,
//!   the host accepts only stored device keys.
//! - Transport: stateful (implicit nonces), so a replayed, dropped, reordered
//!   or altered frame fails to decrypt and the connection must be closed.
//!   Each frame is `[flag: 0 more | 1 final][chunk of one message]`.
//! - Relay authentication: Ed25519 over
//!   `agenthost-relay-v1:{role}:{relay_id}:{nonce}` (see docs/relay-protocol.md).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

pub const PAIR_PATTERN: &str = "Noise_XXpsk3_25519_ChaChaPoly_BLAKE2s";
pub const CONNECT_PATTERN: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
/// Largest Noise message.
pub const MAX_NOISE_MSG: usize = 65535;
/// Plaintext bytes per transport frame (Noise adds a 16-byte tag; one flag byte).
pub const CHUNK: usize = MAX_NOISE_MSG - 16 - 1;
/// Largest reassembled message.
pub const MAX_MESSAGE: usize = 16 * 1024 * 1024;
/// QR code lifetime.
pub const PAIRING_TTL_MS: i64 = 120_000;

#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    #[error("noise: {0}")]
    Noise(#[from] snow::Error),
    #[error("the host key does not match the QR code")]
    HostKeyMismatch,
    #[error("bad frame")]
    BadFrame,
    #[error("message too large")]
    TooLarge,
    #[error("handshake is not finished")]
    NotFinished,
    #[error("invalid: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, TunnelError>;

pub fn b64(b: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(b)
}

pub fn unb64(s: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|e| TunnelError::Invalid(e.to_string()))
}

fn key32(v: &[u8]) -> Result<[u8; 32]> {
    v.try_into()
        .map_err(|_| TunnelError::Invalid("expected 32 bytes".into()))
}

/// An X25519 static key pair for Noise.
#[derive(Clone)]
pub struct StaticKey {
    pub private: [u8; 32],
    pub public: [u8; 32],
}

impl StaticKey {
    pub fn generate() -> StaticKey {
        let kp = snow::Builder::new(CONNECT_PATTERN.parse().unwrap())
            .generate_keypair()
            .expect("keypair");
        StaticKey {
            private: key32(&kp.private).unwrap(),
            public: key32(&kp.public).unwrap(),
        }
    }
    pub fn from_private(private: &[u8]) -> Result<StaticKey> {
        let private = key32(private)?;
        let public = x25519_public(&private);
        Ok(StaticKey { private, public })
    }
}

fn x25519_public(private: &[u8; 32]) -> [u8; 32] {
    x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*private)).to_bytes()
}

/// An Ed25519 key for relay authentication.
#[derive(Clone)]
pub struct RelayKey(pub SigningKey);

impl RelayKey {
    pub fn generate() -> RelayKey {
        RelayKey(SigningKey::generate(&mut rand_core::OsRng))
    }
    pub fn from_bytes(b: &[u8]) -> Result<RelayKey> {
        Ok(RelayKey(SigningKey::from_bytes(&key32(b)?)))
    }
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }
    pub fn public(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }
    /// The relay id for a host with this key.
    pub fn relay_id(&self) -> String {
        relay_id(&self.public())
    }
    /// Sign a relay challenge.
    pub fn sign_challenge(&self, role: &str, relay_id: &str, nonce: &str) -> String {
        b64(&self
            .0
            .sign(challenge_message(role, relay_id, nonce).as_bytes())
            .to_bytes())
    }
    /// Sign a push notification for a relay's push service
    /// (`POST /v1/push/{relay_id}/send`; `time_ms` goes in `x-agenthost-time`).
    pub fn sign_push(&self, time_ms: i64, body: &[u8]) -> String {
        b64(&self
            .0
            .sign(push_send_message(&self.relay_id(), time_ms, body).as_bytes())
            .to_bytes())
    }
}

/// What a host signs to send a push notification through a relay:
/// `agenthost-push-v1:send:{relay_id}:{time_ms}:{base64url(sha256(body))}`.
pub fn push_send_message(relay_id: &str, time_ms: i64, body: &[u8]) -> String {
    format!(
        "agenthost-push-v1:send:{relay_id}:{time_ms}:{}",
        b64(&Sha256::digest(body))
    )
}

/// Verify [`RelayKey::sign_push`] (tests and the dev relay).
pub fn verify_push(public: &[u8], relay_id: &str, time_ms: i64, body: &[u8], sig: &[u8]) -> bool {
    let Ok(pk) = key32(public).and_then(|k| {
        VerifyingKey::from_bytes(&k).map_err(|e| TunnelError::Invalid(e.to_string()))
    }) else {
        return false;
    };
    let Ok(sig) = ed25519_dalek::Signature::from_slice(sig) else {
        return false;
    };
    pk.verify(push_send_message(relay_id, time_ms, body).as_bytes(), &sig)
        .is_ok()
}

pub fn relay_id(public: &[u8; 32]) -> String {
    hex::encode(Sha256::digest(public))[..32].to_string()
}

pub fn challenge_message(role: &str, relay_id: &str, nonce: &str) -> String {
    format!("agenthost-relay-v1:{role}:{relay_id}:{nonce}")
}

/// Verify a relay challenge signature (used by the dev relay).
pub fn verify_challenge(
    public: &[u8],
    role: &str,
    relay_id: &str,
    nonce: &str,
    sig: &[u8],
) -> bool {
    let Ok(pk) = key32(public).and_then(|k| {
        VerifyingKey::from_bytes(&k).map_err(|e| TunnelError::Invalid(e.to_string()))
    }) else {
        return false;
    };
    let Ok(sig) = ed25519_dalek::Signature::from_slice(sig) else {
        return false;
    };
    pk.verify(challenge_message(role, relay_id, nonce).as_bytes(), &sig)
        .is_ok()
}

/// The relay pairing token is derived from the pairing secret, so the relay
/// (which sees the token) never learns the Noise PSK.
pub fn pairing_token(secret: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"agenthost-pair-token-v1");
    h.update(secret);
    h.finalize().into()
}

pub fn sha256(b: &[u8]) -> [u8; 32] {
    Sha256::digest(b).into()
}

/// 6-digit confirmation code from a handshake hash.
pub fn short_code(handshake_hash: &[u8]) -> String {
    let h = Sha256::digest([b"agenthost-short-code-v1".as_slice(), handshake_hash].concat());
    let n = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) % 1_000_000;
    format!("{:03} {:03}", n / 1000, n % 1000)
}

// ---------------------------------------------------------------------------
// QR payload
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct PairingQr {
    pub relay_url: String,
    pub relay_id: String,
    pub host_key: [u8; 32],
    pub secret: [u8; 32],
    pub host_name: String,
}

impl PairingQr {
    pub fn encode(&self) -> String {
        let mut u = url::Url::parse("agenthost://pair").unwrap();
        u.query_pairs_mut()
            .append_pair("v", "1")
            .append_pair("r", &self.relay_url)
            .append_pair("id", &self.relay_id)
            .append_pair("hk", &b64(&self.host_key))
            .append_pair("s", &b64(&self.secret))
            .append_pair("n", &self.host_name);
        u.to_string()
    }

    pub fn decode(s: &str) -> Result<PairingQr> {
        let u = url::Url::parse(s.trim()).map_err(|e| TunnelError::Invalid(e.to_string()))?;
        if u.scheme() != "agenthost" || u.host_str() != Some("pair") {
            return Err(TunnelError::Invalid("not an AgentOS pairing code".into()));
        }
        let q: std::collections::HashMap<String, String> = u.query_pairs().into_owned().collect();
        let get = |k: &str| {
            q.get(k)
                .cloned()
                .ok_or_else(|| TunnelError::Invalid(format!("missing {k}")))
        };
        if get("v")? != "1" {
            return Err(TunnelError::Invalid(
                "unsupported pairing code version".into(),
            ));
        }
        Ok(PairingQr {
            relay_url: get("r")?,
            relay_id: get("id")?,
            host_key: key32(&unb64(&get("hk")?)?)?,
            secret: key32(&unb64(&get("s")?)?)?,
            host_name: q.get("n").cloned().unwrap_or_default(),
        })
    }
}

// ---------------------------------------------------------------------------
// Handshakes
// ---------------------------------------------------------------------------

pub struct Handshake {
    state: snow::HandshakeState,
    expected_remote: Option<[u8; 32]>,
}

impl Handshake {
    /// Phone side of pairing.
    pub fn pair_initiator(local: &StaticKey, qr: &PairingQr) -> Result<Handshake> {
        let state = snow::Builder::new(PAIR_PATTERN.parse()?)
            .local_private_key(&local.private)?
            .psk(3, &qr.secret)?
            .build_initiator()?;
        Ok(Handshake {
            state,
            expected_remote: Some(qr.host_key),
        })
    }

    /// Host side of pairing.
    pub fn pair_responder(host: &StaticKey, secret: &[u8; 32]) -> Result<Handshake> {
        let state = snow::Builder::new(PAIR_PATTERN.parse()?)
            .local_private_key(&host.private)?
            .psk(3, secret)?
            .build_responder()?;
        Ok(Handshake {
            state,
            expected_remote: None,
        })
    }

    /// Phone side of a normal connection.
    pub fn connect_initiator(local: &StaticKey, host_key: &[u8; 32]) -> Result<Handshake> {
        let state = snow::Builder::new(CONNECT_PATTERN.parse()?)
            .local_private_key(&local.private)?
            .remote_public_key(host_key)?
            .build_initiator()?;
        Ok(Handshake {
            state,
            expected_remote: Some(*host_key),
        })
    }

    /// Host side of a normal connection.
    pub fn connect_responder(host: &StaticKey) -> Result<Handshake> {
        let state = snow::Builder::new(CONNECT_PATTERN.parse()?)
            .local_private_key(&host.private)?
            .build_responder()?;
        Ok(Handshake {
            state,
            expected_remote: None,
        })
    }

    pub fn write(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; MAX_NOISE_MSG];
        let n = self.state.write_message(payload, &mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    pub fn read(&mut self, msg: &[u8]) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; MAX_NOISE_MSG];
        let n = self.state.read_message(msg, &mut buf)?;
        buf.truncate(n);
        if let (Some(exp), Some(rs)) = (self.expected_remote, self.state.get_remote_static()) {
            if rs != exp {
                return Err(TunnelError::HostKeyMismatch);
            }
        }
        Ok(buf)
    }

    pub fn is_finished(&self) -> bool {
        self.state.is_handshake_finished()
    }

    pub fn is_my_turn(&self) -> bool {
        self.state.is_my_turn()
    }

    pub fn remote_static(&self) -> Option<[u8; 32]> {
        self.state
            .get_remote_static()
            .and_then(|k| k.try_into().ok())
    }

    pub fn handshake_hash(&self) -> Vec<u8> {
        self.state.get_handshake_hash().to_vec()
    }

    pub fn into_transport(self) -> Result<Transport> {
        if !self.is_finished() {
            return Err(TunnelError::NotFinished);
        }
        Ok(Transport {
            state: self.state.into_transport_mode()?,
            partial: Vec::new(),
        })
    }
}

/// An established tunnel.
pub struct Transport {
    state: snow::TransportState,
    partial: Vec<u8>,
}

impl Transport {
    /// Encrypt one message into one or more frames.
    pub fn seal(&mut self, message: &[u8]) -> Result<Vec<Vec<u8>>> {
        if message.len() > MAX_MESSAGE {
            return Err(TunnelError::TooLarge);
        }
        let chunks: Vec<&[u8]> = if message.is_empty() {
            vec![&[][..]]
        } else {
            message.chunks(CHUNK).collect()
        };
        let last = chunks.len() - 1;
        let mut out = Vec::with_capacity(chunks.len());
        for (i, c) in chunks.into_iter().enumerate() {
            let mut plain = Vec::with_capacity(c.len() + 1);
            plain.push(if i == last { 1 } else { 0 });
            plain.extend_from_slice(c);
            let mut buf = vec![0u8; plain.len() + 16];
            let n = self.state.write_message(&plain, &mut buf)?;
            buf.truncate(n);
            out.push(buf);
        }
        Ok(out)
    }

    /// Decrypt one frame. Returns a whole message once its final chunk arrives.
    /// Any error means the connection must be closed.
    pub fn open(&mut self, frame: &[u8]) -> Result<Option<Vec<u8>>> {
        if frame.len() > MAX_NOISE_MSG || frame.len() < 17 {
            return Err(TunnelError::BadFrame);
        }
        let mut buf = vec![0u8; frame.len()];
        let n = self.state.read_message(frame, &mut buf)?;
        let (flag, body) = buf[..n].split_first().ok_or(TunnelError::BadFrame)?;
        if self.partial.len() + body.len() > MAX_MESSAGE {
            return Err(TunnelError::TooLarge);
        }
        self.partial.extend_from_slice(body);
        match flag {
            0 => Ok(None),
            1 => Ok(Some(std::mem::take(&mut self.partial))),
            _ => Err(TunnelError::BadFrame),
        }
    }
}

/// What the phone tells the host in pairing message 3.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct PairHello {
    pub name: String,
    /// Device relay key (Ed25519 public), base64url.
    pub relay_key: String,
}

/// What the host sends after the user decides (first transport message).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct PairResult {
    pub accepted: bool,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub host_id: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The relay's push service checks this in TypeScript
    /// (`relay/test/push.test.ts`, "verifies a host's signature from Rust").
    #[test]
    fn push_signature_vector() {
        let key = RelayKey::from_bytes(&[7u8; 32]).unwrap();
        let body = br#"{"handle":"h","sealed":"s"}"#;
        let sig = key.sign_push(1_700_000_000_000, body);
        println!("public {} relay_id {} sig {sig}", b64(&key.public()), key.relay_id());
        assert!(verify_push(&key.public(), &key.relay_id(), 1_700_000_000_000, body, &unb64(&sig).unwrap()));
        assert!(!verify_push(&key.public(), &key.relay_id(), 1_700_000_000_001, body, &unb64(&sig).unwrap()));
    }

    fn qr(host: &StaticKey, secret: [u8; 32]) -> PairingQr {
        PairingQr {
            relay_url: "wss://r.example".into(),
            relay_id: "ab".repeat(16),
            host_key: host.public,
            secret,
            host_name: "box".into(),
        }
    }

    #[test]
    fn public_key_derivation_matches_snow() {
        for _ in 0..5 {
            let k = StaticKey::generate();
            assert_eq!(
                StaticKey::from_private(&k.private).unwrap().public,
                k.public
            );
        }
    }

    #[test]
    fn pairing_handshake_and_transport() {
        let host = StaticKey::generate();
        let phone = StaticKey::generate();
        let secret = [7u8; 32];
        let q = qr(&host, secret);
        assert_eq!(PairingQr::decode(&q.encode()).unwrap(), q);

        let mut i = Handshake::pair_initiator(&phone, &q).unwrap();
        let mut r = Handshake::pair_responder(&host, &secret).unwrap();
        let m1 = i.write(&[]).unwrap();
        r.read(&m1).unwrap();
        let m2 = r.write(&[]).unwrap();
        i.read(&m2).unwrap();
        let hello = serde_json::to_vec(&PairHello {
            name: "phone".into(),
            relay_key: "x".into(),
        })
        .unwrap();
        let m3 = i.write(&hello).unwrap();
        let got = r.read(&m3).unwrap();
        assert_eq!(got, hello);
        assert!(i.is_finished() && r.is_finished());
        assert_eq!(r.remote_static().unwrap(), phone.public);
        assert_eq!(
            short_code(&i.handshake_hash()),
            short_code(&r.handshake_hash())
        );

        let mut ti = i.into_transport().unwrap();
        let mut tr = r.into_transport().unwrap();
        let big = vec![b'a'; CHUNK * 2 + 10];
        let frames = ti.seal(&big).unwrap();
        assert_eq!(frames.len(), 3);
        let mut out = None;
        for f in &frames {
            out = tr.open(f).unwrap();
        }
        assert_eq!(out.unwrap(), big);
        // Replay of an old frame fails.
        assert!(tr.open(&frames[0]).is_err());
    }

    #[test]
    fn wrong_psk_or_host_key_fails() {
        let host = StaticKey::generate();
        let phone = StaticKey::generate();
        let mut i = Handshake::pair_initiator(&phone, &qr(&host, [1; 32])).unwrap();
        let mut r = Handshake::pair_responder(&host, &[2; 32]).unwrap();
        let m1 = i.write(&[]).unwrap();
        r.read(&m1).unwrap();
        let m2 = r.write(&[]).unwrap();
        i.read(&m2).unwrap();
        let m3 = i.write(&[]).unwrap();
        assert!(r.read(&m3).is_err(), "wrong PSK must fail");

        let impostor = StaticKey::generate();
        let mut i = Handshake::pair_initiator(&phone, &qr(&host, [1; 32])).unwrap();
        let mut r = Handshake::pair_responder(&impostor, &[1; 32]).unwrap();
        let m1 = i.write(&[]).unwrap();
        r.read(&m1).unwrap();
        let m2 = r.write(&[]).unwrap();
        assert!(matches!(i.read(&m2), Err(TunnelError::HostKeyMismatch)));
    }

    #[test]
    fn ik_connect_and_tamper() {
        let host = StaticKey::generate();
        let phone = StaticKey::generate();
        let mut i = Handshake::connect_initiator(&phone, &host.public).unwrap();
        let mut r = Handshake::connect_responder(&host).unwrap();
        let m1 = i.write(&[]).unwrap();
        r.read(&m1).unwrap();
        assert_eq!(r.remote_static().unwrap(), phone.public);
        let m2 = r.write(&[]).unwrap();
        i.read(&m2).unwrap();
        let mut ti = i.into_transport().unwrap();
        let mut tr = r.into_transport().unwrap();
        let mut f = ti.seal(b"hello").unwrap().remove(0);
        f[5] ^= 1;
        assert!(tr.open(&f).is_err(), "altered frame must fail");
    }

    #[test]
    fn relay_signatures() {
        let k = RelayKey::generate();
        let id = k.relay_id();
        assert_eq!(id.len(), 32);
        let sig = k.sign_challenge("host", &id, "bm9uY2U");
        assert!(verify_challenge(
            &k.public(),
            "host",
            &id,
            "bm9uY2U",
            &unb64(&sig).unwrap()
        ));
        assert!(!verify_challenge(
            &k.public(),
            "device",
            &id,
            "bm9uY2U",
            &unb64(&sig).unwrap()
        ));
        assert_ne!(pairing_token(&[1; 32]), [1; 32]);
        assert_eq!(short_code(b"x").len(), 7);
    }
}
