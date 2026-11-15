// Encoding and crypto helpers. All binary values in JSON are base64url
// without padding (RFC 4648 section 5, no "=").

const B64URL_RE = /^[A-Za-z0-9_-]*$/;

export function b64urlEncode(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]!);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** Strict decode: rejects padding, non-url alphabet and impossible lengths. */
export function b64urlDecode(s: unknown): Uint8Array | null {
  if (typeof s !== "string" || !B64URL_RE.test(s) || s.length % 4 === 1) return null;
  const pad = s.length % 4 === 0 ? "" : "=".repeat(4 - (s.length % 4));
  let bin: string;
  try {
    bin = atob(s.replace(/-/g, "+").replace(/_/g, "/") + pad);
  } catch {
    return null;
  }
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  // Reject non-canonical encodings (non-zero trailing bits).
  if (b64urlEncode(out) !== s) return null;
  return out;
}

/** Decodes a base64url value that must be exactly `len` bytes. */
export function decodeFixed(s: unknown, len: number): Uint8Array | null {
  const b = b64urlDecode(s);
  return b && b.length === len ? b : null;
}

export function hex(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i++) out += bytes[i]!.toString(16).padStart(2, "0");
  return out;
}

export async function sha256(bytes: Uint8Array): Promise<Uint8Array> {
  return new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
}

/** relay_id = first 32 lowercase hex chars of SHA-256(host relay public key). */
export async function relayIdFor(hostPublicKey: Uint8Array): Promise<string> {
  return hex(await sha256(hostPublicKey)).slice(0, 32);
}

export function signedMessage(role: string, relayId: string, nonce: string): Uint8Array {
  return new TextEncoder().encode(`agenthost-relay-v1:${role}:${relayId}:${nonce}`);
}

export async function verifyEd25519(
  publicKey: Uint8Array,
  signature: Uint8Array,
  message: Uint8Array,
): Promise<boolean> {
  try {
    const key = await crypto.subtle.importKey("raw", publicKey, { name: "Ed25519" }, false, [
      "verify",
    ]);
    return await crypto.subtle.verify("Ed25519", key, signature, message);
  } catch {
    return false;
  }
}

export function randomBytes(n: number): Uint8Array {
  const b = new Uint8Array(n);
  crypto.getRandomValues(b);
  return b;
}
