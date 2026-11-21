// Sealing webhook deliveries to the host (docs/relay-protocol.md, "Webhooks").
//
//   eph            = fresh X25519 key pair
//   shared         = X25519(eph.private, seal_key)
//   key            = HKDF-SHA256(ikm = shared, salt = eph_pub || seal_key,
//                                info = "agenthost-hook-v1", 32 bytes)
//   ct             = AES-256-GCM(key, nonce (12 random bytes), plaintext,
//                                aad = UTF-8 delivery_id)   (16-byte tag appended)
//   sealed         = eph_pub (32) || nonce (12) || ct
//
// WebCrypto only; nothing here is stored.

export const SEAL_INFO = "agenthost-hook-v1";
export const SEAL_KEY_LEN = 32;
export const SEAL_NONCE_LEN = 12;

const enc = new TextEncoder();

export async function sealToHost(
  sealKey: Uint8Array,
  deliveryId: string,
  plaintext: Uint8Array,
): Promise<Uint8Array> {
  if (sealKey.length !== SEAL_KEY_LEN) throw new Error("seal key must be 32 bytes");
  const hostKey = await crypto.subtle.importKey("raw", sealKey, { name: "X25519" }, false, []);
  const eph = (await crypto.subtle.generateKey({ name: "X25519" }, true, [
    "deriveBits",
  ])) as CryptoKeyPair;
  const ephPub = new Uint8Array((await crypto.subtle.exportKey("raw", eph.publicKey)) as ArrayBuffer);
  // workers-types spells the `public` member `$public`; the runtime wants `public`.
  const shared = await crypto.subtle.deriveBits(
    { name: "X25519", public: hostKey } as unknown as SubtleCryptoDeriveKeyAlgorithm,
    eph.privateKey,
    256,
  );

  const salt = new Uint8Array(SEAL_KEY_LEN * 2);
  salt.set(ephPub, 0);
  salt.set(sealKey, SEAL_KEY_LEN);
  const ikm = await crypto.subtle.importKey("raw", shared, "HKDF", false, ["deriveBits"]);
  const okm = await crypto.subtle.deriveBits(
    { name: "HKDF", hash: "SHA-256", salt, info: enc.encode(SEAL_INFO) },
    ikm,
    256,
  );
  const key = await crypto.subtle.importKey("raw", okm, { name: "AES-GCM" }, false, ["encrypt"]);

  const nonce = new Uint8Array(SEAL_NONCE_LEN);
  crypto.getRandomValues(nonce);
  const ct = new Uint8Array(
    await crypto.subtle.encrypt(
      { name: "AES-GCM", iv: nonce, additionalData: enc.encode(deliveryId) },
      key,
      plaintext,
    ),
  );

  const out = new Uint8Array(SEAL_KEY_LEN + SEAL_NONCE_LEN + ct.length);
  out.set(ephPub, 0);
  out.set(nonce, SEAL_KEY_LEN);
  out.set(ct, SEAL_KEY_LEN + SEAL_NONCE_LEN);
  return out;
}
