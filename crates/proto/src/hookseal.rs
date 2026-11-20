//! Webhook deliveries sealed by the relay to the host.
//!
//! The relay seals each incoming webhook request the moment it arrives, so
//! it only ever stores and forwards ciphertext:
//!
//! ```text
//! shared = X25519(ephemeral, host seal key)
//! key    = HKDF-SHA256(ikm = shared, salt = eph_pub || seal_key, info = "agenthost-hook-v1")
//! sealed = eph_pub (32) || nonce (12) || AES-256-GCM(key, nonce, envelope, aad = delivery_id)
//! ```
//!
//! The relay Worker implements [`seal`] with WebCrypto (`relay/src`); see
//! docs/relay-protocol.md.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

pub const INFO: &[u8] = b"agenthost-hook-v1";

/// What the relay seals: the request as it arrived.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Envelope {
    pub v: u32,
    pub hook_id: String,
    pub delivery_id: String,
    pub received_at: i64,
    /// Lowercased names, in arrival order.
    pub headers: Vec<(String, String)>,
    /// The raw body, base64url without padding.
    pub body: String,
}

impl Envelope {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn body_bytes(&self) -> Option<Vec<u8>> {
        crate::tunnel::unb64(&self.body).ok()
    }
}

fn key(shared: &[u8; 32], eph_pub: &[u8; 32], seal_key: &[u8; 32]) -> Aes256Gcm {
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(eph_pub);
    salt[32..].copy_from_slice(seal_key);
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared);
    let mut okm = [0u8; 32];
    hk.expand(INFO, &mut okm).expect("32 bytes is a valid HKDF length");
    Aes256Gcm::new_from_slice(&okm).expect("32-byte key")
}

/// Seal `plaintext` to `seal_key` (what the relay does).
pub fn seal(seal_key: &[u8; 32], delivery_id: &str, plaintext: &[u8]) -> Vec<u8> {
    let eph = StaticSecret::random_from_rng(OsRng);
    let eph_pub = PublicKey::from(&eph).to_bytes();
    let shared = eph.diffie_hellman(&PublicKey::from(*seal_key)).to_bytes();
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let ct = key(&shared, &eph_pub, seal_key)
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: plaintext, aad: delivery_id.as_bytes() })
        .expect("encryption does not fail");
    let mut out = Vec::with_capacity(44 + ct.len());
    out.extend_from_slice(&eph_pub);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

/// Open a sealed delivery with the host's seal key.
pub fn open(secret: &StaticSecret, delivery_id: &str, sealed: &[u8]) -> Result<Vec<u8>, &'static str> {
    if sealed.len() < 32 + 12 + 16 {
        return Err("sealed delivery too short");
    }
    let eph_pub: [u8; 32] = sealed[..32].try_into().unwrap();
    let seal_key = PublicKey::from(secret).to_bytes();
    let shared = secret.diffie_hellman(&PublicKey::from(eph_pub));
    if !shared.was_contributory() {
        return Err("bad ephemeral key");
    }
    key(shared.as_bytes(), &eph_pub, &seal_key)
        .decrypt(Nonce::from_slice(&sealed[32..44]), Payload { msg: &sealed[44..], aad: delivery_id.as_bytes() })
        .map_err(|_| "the delivery does not open with this host's key")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sealed by the relay's `sealToHost` (relay/src/seal.ts) under Node's
    /// WebCrypto, to the key whose raw bytes are 1..=32.
    #[test]
    fn opens_what_the_worker_sealed() {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = i as u8 + 1;
        }
        let secret = StaticSecret::from(k);
        assert_eq!(
            crate::tunnel::b64(PublicKey::from(&secret).as_bytes()),
            "B6N8vBQgk8i3VdwbEOhstCY3StFqqFPtC9_AsrhtHHw"
        );
        let sealed = crate::tunnel::unb64(
            "0H_2i_ZqDEvv6PCWWTjSiH_v4nTyd6MwJWW4beHzRBTY38LojpI04AeFVr9pFh6DVTzawfz3kYaGVdPJE7K6oA92kNS1KupUprdYv5E4R9RYiOM",
        )
        .unwrap();
        let plain = open(&secret, "0001700000000000-00112233aabbccdd", &sealed).unwrap();
        assert_eq!(plain, br#"{"v":1,"hello":"world"}"#);
    }

    #[test]
    fn round_trip_and_tamper() {
        let secret = StaticSecret::random_from_rng(OsRng);
        let pubk = PublicKey::from(&secret).to_bytes();
        let sealed = seal(&pubk, "d1", b"hello");
        assert_eq!(open(&secret, "d1", &sealed).unwrap(), b"hello");
        assert!(open(&secret, "d2", &sealed).is_err(), "delivery id is bound");
        let mut bad = sealed.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(open(&secret, "d1", &bad).is_err());
        let other = StaticSecret::random_from_rng(OsRng);
        assert!(open(&other, "d1", &sealed).is_err());
    }
}
