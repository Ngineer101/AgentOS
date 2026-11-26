// Push notifications through Apple's APNs, for hosts that don't have an APNs
// key of their own.
//
// Apple only takes notifications for an app from the Apple Developer team
// that signs it, so whoever publishes the iOS app runs this, with that
// team's APNs key as a Worker secret. Without the secrets these routes answer
// 404 and phones fall back to a host with its own key.
//
// - `POST /v1/push/register` (the phone): its APNs device token, sealed
//   with a key only this relay has and bound to one host's relay id. The
//   phone gives the host this "handle", never the token.
// - `POST /v1/push/{relay_id}/send` (the host, signed with its relay key):
//   a handle and the notification's content, already sealed to the phone.
//   The relay adds the generic alert Apple shows and sends it.
//
// Nothing is stored: handles are self-contained, and what a notification
// says is opened only on the phone.

import { b64urlDecode, b64urlEncode, decodeFixed, relayIdFor, sha256, verifyEd25519 } from "./codec";

export interface PushEnv {
  /** The `.p8` key (PEM), a secret. */
  APNS_KEY?: string;
  /** Its 10-character key id. */
  APNS_KEY_ID?: string;
  /** The Apple Developer team id. */
  APNS_TEAM_ID?: string;
  /** The app's bundle id, the only APNs topic this relay sends for. */
  APNS_TOPIC?: string;
  /** Tests only: an APNs stand-in; requests go to `<url>/<environment>/3/device/…`. */
  APNS_URL?: string;
}

export type ApnsEnvironment = "sandbox" | "production";

/** What Apple, and a phone that can't open the content, shows. */
export const GENERIC_TITLE = "AgentOS";
export const GENERIC_BODY = "A session needs you";

/** How far a host's clock may be from ours. */
export const SEND_CLOCK_SKEW_MS = 5 * 60 * 1000;
/** Notifications per host per minute. */
export const SEND_RATE_LIMIT = 60;
export const SEND_RATE_WINDOW_MS = 60_000;
/** Longest sealed content (base64url); APNs allows 4 KB for the whole payload. */
export const MAX_SEALED = 3000;
export const MAX_SEND_BODY = 8192;
/** How long APNs keeps trying a phone that is off. */
const EXPIRY_S = 60 * 60;
/** Provider tokens are reused for this long (APNs: renew every 20–60 minutes). */
const TOKEN_PERIOD_S = 40 * 60;

const HANDLE_INFO = "agenthost-push-handle-v1";
const enc = new TextEncoder();
const dec = new TextDecoder();

const PRODUCTION_URL = "https://api.push.apple.com";
const SANDBOX_URL = "https://api.sandbox.push.apple.com";

interface Credentials {
  pem: string;
  keyId: string;
  teamId: string;
  topic: string;
}

export function credentials(env: PushEnv): Credentials | null {
  const pem = env.APNS_KEY?.trim();
  const keyId = env.APNS_KEY_ID?.trim();
  const teamId = env.APNS_TEAM_ID?.trim();
  const topic = env.APNS_TOPIC?.trim();
  if (!pem || !keyId || !teamId || !topic) return null;
  return { pem, keyId, teamId, topic };
}

/** What a handle holds. */
interface Handle {
  v: 1;
  /** Device token, lowercase hex. */
  t: string;
  e: ApnsEnvironment;
  /** The host that may send to it. */
  r: string;
}

/** The message a host signs for `send` (`agenthost-push-v1:send:<relay_id>:<ms>:<sha256(body)>`). */
export function sendMessage(relayId: string, time: string, bodyHash: Uint8Array): Uint8Array {
  return enc.encode(`agenthost-push-v1:send:${relayId}:${time}:${b64urlEncode(bodyHash)}`);
}

// ------------------------------------------------------------------ keys

function pemBody(pem: string): Uint8Array | null {
  const b64 = pem
    .replace(/-----(BEGIN|END) PRIVATE KEY-----/g, "")
    .replace(/\s+/g, "")
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
  return b64urlDecode(b64);
}

const signingKeys = new Map<string, Promise<CryptoKey>>();
const handleKeys = new Map<string, Promise<CryptoKey>>();

function signingKey(c: Credentials): Promise<CryptoKey> {
  let k = signingKeys.get(c.pem);
  if (!k) {
    const der = pemBody(c.pem);
    k = der
      ? crypto.subtle.importKey("pkcs8", der, { name: "ECDSA", namedCurve: "P-256" }, false, ["sign"])
      : Promise.reject(new Error("APNS_KEY is not a PEM private key"));
    k.catch(() => signingKeys.delete(c.pem));
    signingKeys.set(c.pem, k);
  }
  return k;
}

/** AES-256-GCM key for handles, derived from the APNs key (one secret fewer to keep). */
function handleKey(c: Credentials): Promise<CryptoKey> {
  let k = handleKeys.get(c.pem);
  if (!k) {
    k = (async () => {
      const ikm = await crypto.subtle.importKey("raw", enc.encode(c.pem), "HKDF", false, ["deriveKey"]);
      return crypto.subtle.deriveKey(
        { name: "HKDF", hash: "SHA-256", salt: new Uint8Array(0), info: enc.encode(HANDLE_INFO) },
        ikm,
        { name: "AES-GCM", length: 256 },
        false,
        ["encrypt", "decrypt"],
      );
    })();
    k.catch(() => handleKeys.delete(c.pem));
    handleKeys.set(c.pem, k);
  }
  return k;
}

function handleAad(c: Credentials): Uint8Array {
  return enc.encode(`${HANDLE_INFO}:${c.topic}`);
}

export async function sealHandle(c: Credentials, h: Handle): Promise<string> {
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const ct = new Uint8Array(
    await crypto.subtle.encrypt(
      { name: "AES-GCM", iv, additionalData: handleAad(c) },
      await handleKey(c),
      enc.encode(JSON.stringify(h)),
    ),
  );
  const out = new Uint8Array(12 + ct.length);
  out.set(iv);
  out.set(ct, 12);
  return b64urlEncode(out);
}

export async function openHandle(c: Credentials, handle: unknown): Promise<Handle | null> {
  if (typeof handle !== "string" || handle.length > 1024) return null;
  const raw = b64urlDecode(handle);
  if (!raw || raw.length < 12 + 16) return null;
  try {
    const plain = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: raw.subarray(0, 12), additionalData: handleAad(c) },
      await handleKey(c),
      raw.subarray(12),
    );
    const h = JSON.parse(dec.decode(plain)) as Handle;
    if (h.v !== 1 || !validToken(h.t) || !validEnvironment(h.e) || !RELAY_ID.test(h.r)) return null;
    return h;
  } catch {
    return null;
  }
}

// --------------------------------------------------------------- tokens

const RELAY_ID = /^[0-9a-f]{32}$/;

function validToken(t: unknown): t is string {
  return typeof t === "string" && /^[0-9a-f]{32,400}$/.test(t) && t.length % 2 === 0;
}

function validEnvironment(e: unknown): e is ApnsEnvironment {
  return e === "sandbox" || e === "production";
}

/** ES256 provider token. `iat` is rounded so every instance uses the same one. */
/** The APNs key secret parses as a P-256 private key. */
async function keyUsable(c: Credentials): Promise<boolean> {
  try {
    await signingKey(c);
    return true;
  } catch {
    return false;
  }
}

export async function providerToken(c: Credentials, nowS: number): Promise<string> {
  const iat = nowS - (nowS % TOKEN_PERIOD_S);
  const header = b64urlEncode(enc.encode(JSON.stringify({ alg: "ES256", kid: c.keyId })));
  const claims = b64urlEncode(enc.encode(JSON.stringify({ iss: c.teamId, iat })));
  const input = `${header}.${claims}`;
  const sig = await crypto.subtle.sign(
    { name: "ECDSA", hash: "SHA-256" },
    await signingKey(c),
    enc.encode(input),
  );
  return `${input}.${b64urlEncode(new Uint8Array(sig))}`;
}

// --------------------------------------------------------------- routes

function reply(status: number, body: Record<string, unknown>, headers?: HeadersInit): Response {
  const h = new Headers(headers);
  h.set("content-type", "application/json");
  return new Response(JSON.stringify(body), { status, headers: h });
}

async function readJson(request: Request, limit: number): Promise<Record<string, unknown> | null> {
  const text = await readText(request, limit);
  if (text === null) return null;
  try {
    const v = JSON.parse(text);
    return v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

async function readText(request: Request, limit: number): Promise<string | null> {
  const declared = request.headers.get("content-length");
  if (declared !== null && !(Number(declared) <= limit)) return null;
  const buf = new Uint8Array(await request.arrayBuffer());
  return buf.length <= limit ? dec.decode(buf) : null;
}

/**
 * `POST /v1/push/register` `{token, topic, environment, relay_id}` →
 * `{handle}`. Open to anyone: a handle only lets the host it names send to
 * the token in it, and only someone who has the token can make one.
 */
export async function register(request: Request, env: PushEnv): Promise<Response> {
  if (request.method !== "POST") return reply(405, { error: "method not allowed" }, { allow: "POST" });
  const c = credentials(env);
  if (!c) return reply(404, { error: "this relay does not send notifications" });
  const msg = await readJson(request, 4096);
  if (!msg) return reply(400, { error: "expected a JSON object" });
  const token = typeof msg.token === "string" ? msg.token.trim().toLowerCase() : null;
  if (!validToken(token)) return reply(400, { error: "token: the APNs device token, hex" });
  if (!validEnvironment(msg.environment)) return reply(400, { error: "environment: sandbox or production" });
  if (typeof msg.relay_id !== "string" || !RELAY_ID.test(msg.relay_id)) {
    return reply(400, { error: "relay_id: the host's relay id" });
  }
  if (msg.topic !== c.topic) {
    // Another build of the app (another bundle id): its own team's key is needed.
    return reply(403, {
      error: `this relay sends notifications for ${c.topic} only, not ${String(msg.topic).slice(0, 200)}`,
    });
  }
  if (!(await keyUsable(c))) {
    return reply(503, { error: "this relay's APNS_KEY is not a valid .p8 key" });
  }
  const handle = await sealHandle(c, { v: 1, t: token, e: msg.environment, r: msg.relay_id });
  return reply(200, { handle });
}

/** In-memory state of the Durable Object `send` runs in (one per host). */
export class PushSender {
  private hits: number[] = [];

  constructor(private readonly env: PushEnv) {}

  /**
   * `POST /v1/push/{relay_id}/send`, headers `x-agenthost-key` (the host's
   * relay public key), `x-agenthost-time` (unix ms) and
   * `x-agenthost-signature` (Ed25519 over `sendMessage`), body
   * `{handle, sealed, collapse_id?}`. Answers as APNs would: 200, or a
   * status and `{reason}` (`Unregistered`, `BadDeviceToken`, …, or this
   * relay's own `BadSignature`, `BadHandle`, `TooManyRequests`,
   * `PushUnavailable`).
   */
  async send(request: Request, relayId: string): Promise<Response> {
    if (request.method !== "POST") return reply(405, { reason: "MethodNotAllowed" }, { allow: "POST" });
    const c = credentials(this.env);
    if (!c) return reply(404, { reason: "PushUnavailable" });

    const text = await readText(request, MAX_SEND_BODY);
    if (text === null) return reply(413, { reason: "PayloadTooLarge" });
    const key = decodeFixed(request.headers.get("x-agenthost-key"), 32);
    const sig = decodeFixed(request.headers.get("x-agenthost-signature"), 64);
    const time = request.headers.get("x-agenthost-time") ?? "";
    if (!key || !sig || !/^\d{1,16}$/.test(time)) return reply(401, { reason: "BadSignature" });
    if (Math.abs(Date.now() - Number(time)) > SEND_CLOCK_SKEW_MS) {
      return reply(401, { reason: "BadSignature", error: "the host's clock is off" });
    }
    const body = enc.encode(text);
    if (
      (await relayIdFor(key)) !== relayId ||
      !(await verifyEd25519(key, sig, sendMessage(relayId, time, await sha256(body))))
    ) {
      return reply(401, { reason: "BadSignature" });
    }

    let msg: Record<string, unknown> | null = null;
    try {
      const v = JSON.parse(text);
      if (v && typeof v === "object" && !Array.isArray(v)) msg = v as Record<string, unknown>;
    } catch {
      // below
    }
    if (!msg) return reply(400, { reason: "BadPayload" });
    const handle = await openHandle(c, msg.handle);
    if (!handle || handle.r !== relayId) return reply(400, { reason: "BadHandle" });
    const sealed = msg.sealed;
    if (typeof sealed !== "string" || sealed.length > MAX_SEALED || !b64urlDecode(sealed)) {
      return reply(400, { reason: "BadPayload" });
    }
    const collapse = msg.collapse_id;
    if (collapse !== undefined && (typeof collapse !== "string" || enc.encode(collapse).length > 64)) {
      return reply(400, { reason: "BadCollapseId" });
    }
    if (!this.allowed()) return reply(429, { reason: "TooManyRequests" }, { "retry-after": "60" });

    const payload = JSON.stringify({
      aps: {
        alert: { title: GENERIC_TITLE, body: GENERIC_BODY },
        sound: "default",
        "mutable-content": 1,
      },
      v: 1,
      e: sealed,
    });
    let environment = handle.e;
    let r = await this.post(c, handle.t, environment, payload, collapse as string | undefined);
    if (r.reason === "BadDeviceToken") {
      // A build that got its environment wrong (a debug build that thinks it
      // is a release, or the other way round).
      const other: ApnsEnvironment = environment === "sandbox" ? "production" : "sandbox";
      const again = await this.post(c, handle.t, other, payload, collapse as string | undefined);
      if (again.status === 200) {
        environment = other;
        r = again;
      }
    }
    if (r.status === 200) return reply(200, { environment });
    return reply(r.status, { reason: r.reason });
  }

  /** Sliding-window rate limit per host; best effort (resets on eviction). */
  private allowed(): boolean {
    const now = Date.now();
    this.hits = this.hits.filter((t) => t > now - SEND_RATE_WINDOW_MS);
    if (this.hits.length >= SEND_RATE_LIMIT) return false;
    this.hits.push(now);
    return true;
  }

  private base(env: ApnsEnvironment): string {
    if (this.env.APNS_URL) return `${this.env.APNS_URL.replace(/\/+$/, "")}/${env}`;
    return env === "production" ? PRODUCTION_URL : SANDBOX_URL;
  }

  private async post(
    c: Credentials,
    token: string,
    env: ApnsEnvironment,
    payload: string,
    collapse: string | undefined,
  ): Promise<{ status: number; reason: string }> {
    const nowS = Math.floor(Date.now() / 1000);
    let jwt: string;
    try {
      jwt = await providerToken(c, nowS);
    } catch {
      // APNS_KEY isn't a .p8 private key: Apple was never asked.
      return { status: 500, reason: "RelayKeyInvalid" };
    }
    const headers: Record<string, string> = {
      authorization: `bearer ${jwt}`,
      "apns-topic": c.topic,
      "apns-push-type": "alert",
      "apns-priority": "10",
      "apns-expiration": String(nowS + EXPIRY_S),
      "content-type": "application/json",
    };
    if (collapse) headers["apns-collapse-id"] = collapse;
    let resp: Response;
    try {
      resp = await fetch(`${this.base(env)}/3/device/${token}`, { method: "POST", headers, body: payload });
    } catch {
      return { status: 502, reason: "ApnsUnreachable" };
    }
    if (resp.status === 200) {
      await resp.body?.cancel();
      return { status: 200, reason: "" };
    }
    let reason = "Unknown";
    try {
      const v = (await resp.json()) as { reason?: unknown };
      if (typeof v.reason === "string") reason = v.reason;
    } catch {
      // keep "Unknown"
    }
    // 5xx from Apple is Apple's trouble, not the host's: say so plainly.
    return { status: resp.status >= 500 ? 502 : resp.status, reason };
  }
}
