import { exports } from "cloudflare:workers";
import { b64urlDecode, b64urlEncode, relayIdFor, sha256, signedMessage } from "../src/codec";
import { SEAL_INFO } from "../src/seal";

export const BASE = "https://relay.test";

export interface Identity {
  publicKey: string; // base64url
  raw: Uint8Array;
  privateKey: CryptoKey;
}

export async function newIdentity(): Promise<Identity> {
  const kp = (await crypto.subtle.generateKey({ name: "Ed25519" }, true, [
    "sign",
    "verify",
  ])) as CryptoKeyPair;
  const raw = new Uint8Array(
    (await crypto.subtle.exportKey("raw", kp.publicKey)) as ArrayBuffer,
  );
  return { publicKey: b64urlEncode(raw), raw, privateKey: kp.privateKey };
}

export async function sign(
  id: Identity,
  role: string,
  relayId: string,
  nonce: string,
): Promise<string> {
  const sig = await crypto.subtle.sign("Ed25519", id.privateKey, signedMessage(role, relayId, nonce));
  return b64urlEncode(new Uint8Array(sig));
}

export async function relayId(host: Identity): Promise<string> {
  return relayIdFor(host.raw);
}

export type Msg = string | ArrayBuffer;

/** Test-side WebSocket with an awaitable message queue. */
export class Client {
  // Each entry resolves to the frame; Blobs (workerd's default binary type on
  // the client side) are converted while keeping arrival order.
  private queue: Promise<Msg>[] = [];
  private waiters: ((m: Promise<Msg>) => void)[] = [];
  readonly closed: Promise<{ code: number; reason: string }>;

  constructor(readonly ws: WebSocket) {
    ws.accept();
    ws.addEventListener("message", (e) => {
      const d = e.data as unknown;
      const data: Promise<Msg> =
        d instanceof Blob ? d.arrayBuffer() : Promise.resolve(d as Msg);
      const w = this.waiters.shift();
      if (w) w(data);
      else this.queue.push(data);
    });
    this.closed = new Promise((resolve) => {
      ws.addEventListener("close", (e) => resolve({ code: e.code, reason: e.reason }));
    });
  }

  next(timeoutMs = 2000): Promise<Msg> {
    const m = this.queue.shift();
    if (m !== undefined) return m;
    return new Promise((resolve, reject) => {
      const t = setTimeout(() => {
        const i = this.waiters.indexOf(waiter);
        if (i >= 0) this.waiters.splice(i, 1);
        reject(new Error("timed out waiting for message"));
      }, timeoutMs);
      const waiter = (msg: Promise<Msg>) => {
        clearTimeout(t);
        resolve(msg);
      };
      this.waiters.push(waiter);
    });
  }

  async json(): Promise<Record<string, unknown>> {
    const m = await this.next();
    if (typeof m !== "string") throw new Error("expected text frame");
    return JSON.parse(m);
  }

  async binary(): Promise<Uint8Array> {
    const m = await this.next();
    if (typeof m === "string") throw new Error(`expected binary frame, got ${m}`);
    return new Uint8Array(m);
  }

  /** Resolves true if no message arrives within `ms`. */
  async silent(ms = 200): Promise<boolean> {
    try {
      await this.next(ms);
      return false;
    } catch {
      return true;
    }
  }

  send(m: string | Uint8Array | Record<string, unknown>): void {
    if (typeof m === "string" || m instanceof Uint8Array) this.ws.send(m);
    else this.ws.send(JSON.stringify(m));
  }

  async closeCode(): Promise<number> {
    return (await this.closed).code;
  }
}

export async function connect(path: string): Promise<Client> {
  const res = await exports.default.fetch(`${BASE}${path}`, {
    headers: { Upgrade: "websocket" },
  });
  if (res.status !== 101 || !res.webSocket) throw new Error(`upgrade failed: ${res.status}`);
  return new Client(res.webSocket);
}

/** Connects and returns the client plus the challenge nonce. */
export async function open(rid: string, role: string): Promise<{ c: Client; nonce: string }> {
  const c = await connect(`/v1/relay/${rid}/${role}`);
  const ch = await c.json();
  if (ch.type !== "challenge" || typeof ch.nonce !== "string") throw new Error("no challenge");
  return { c, nonce: ch.nonce };
}

/** Authenticates a host socket and returns right after its `ready`. */
export async function authHost(host: Identity) {
  const rid = await relayId(host);
  const { c, nonce } = await open(rid, "host");
  c.send({
    type: "auth",
    role: "host",
    public_key: host.publicKey,
    signature: await sign(host, "host", rid, nonce),
  });
  const ready = await c.json();
  if (ready.type !== "ready") throw new Error(`host not ready: ${JSON.stringify(ready)}`);
  return { c, rid };
}

export async function connectHost(host: Identity, devices: Identity[] = []) {
  const { c, rid } = await authHost(host);
  c.send({ type: "devices.set", public_keys: devices.map((d) => d.publicKey) });
  await ping(c);
  return { c, rid };
}

/** Round-trips a ping so earlier control messages are known to be processed. */
export async function ping(host: Client): Promise<void> {
  host.send('{ "type": "ping" }'); // not the exact auto-response string: goes through the handler
  const m = await host.json();
  if (m.type !== "pong") throw new Error(`expected pong, got ${JSON.stringify(m)}`);
}

export async function connectDevice(rid: string, dev: Identity): Promise<Client> {
  const { c, nonce } = await open(rid, "device");
  c.send({
    type: "auth",
    role: "device",
    public_key: dev.publicKey,
    signature: await sign(dev, "device", rid, nonce),
  });
  return c;
}

export async function connectPair(rid: string, token: Uint8Array): Promise<Client> {
  const { c } = await open(rid, "pair");
  c.send({ type: "auth", role: "pair", pairing_token: b64urlEncode(token) });
  return c;
}

export async function tokenHash(token: Uint8Array): Promise<string> {
  return b64urlEncode(await sha256(token));
}

export function frame(connId: string, payload: Uint8Array): Uint8Array {
  const id = new TextEncoder().encode(connId);
  const out = new Uint8Array(1 + id.length + payload.length);
  out[0] = id.length;
  out.set(id, 1);
  out.set(payload, 1 + id.length);
  return out;
}

export function unframe(f: Uint8Array): { connId: string; payload: Uint8Array } {
  const n = f[0]!;
  return { connId: new TextDecoder().decode(f.subarray(1, 1 + n)), payload: f.slice(1 + n) };
}

// ------------------------------------------------------------- webhooks

export interface SealKey {
  publicKey: string; // base64url
  raw: Uint8Array;
  privateKey: CryptoKey;
}

export async function newSealKey(): Promise<SealKey> {
  const kp = (await crypto.subtle.generateKey({ name: "X25519" }, true, [
    "deriveBits",
  ])) as CryptoKeyPair;
  const raw = new Uint8Array((await crypto.subtle.exportKey("raw", kp.publicKey)) as ArrayBuffer);
  return { publicKey: b64urlEncode(raw), raw, privateKey: kp.privateKey };
}

/** A fresh, well-formed hook id (32 base64url characters). */
export function newHookId(): string {
  const b = new Uint8Array(24);
  crypto.getRandomValues(b);
  return b64urlEncode(b);
}

export async function hookHash(hookId: string): Promise<string> {
  return b64urlEncode(await sha256(new TextEncoder().encode(hookId)));
}

export async function setHooks(host: Client, seal: SealKey, hookIds: string[]): Promise<void> {
  host.send({
    type: "hooks.set",
    seal_key: seal.publicKey,
    hooks: await Promise.all(hookIds.map(hookHash)),
  });
  await ping(host);
}

export function postHook(
  rid: string,
  hookId: string,
  body: BodyInit = "{}",
  headers: Record<string, string> = { "content-type": "application/json" },
): Promise<Response> {
  return exports.default.fetch(`${BASE}/v1/hook/${rid}/${hookId}`, {
    method: "POST",
    headers,
    body,
  });
}

/** The host side of sealToHost (what the Rust host does). Throws if it does not open. */
export async function openFromRelay(
  privateKey: CryptoKey,
  sealKeyRaw: Uint8Array,
  deliveryId: string,
  sealed: Uint8Array,
): Promise<Uint8Array> {
  if (sealed.length < 32 + 12 + 16) throw new Error("sealed blob too short");
  const ephPub = sealed.slice(0, 32);
  const nonce = sealed.slice(32, 44);
  const ct = sealed.slice(44);
  const eph = await crypto.subtle.importKey("raw", ephPub, { name: "X25519" }, false, []);
  const shared = await crypto.subtle.deriveBits(
    { name: "X25519", public: eph } as unknown as SubtleCryptoDeriveKeyAlgorithm,
    privateKey,
    256,
  );
  const salt = new Uint8Array(64);
  salt.set(ephPub, 0);
  salt.set(sealKeyRaw, 32);
  const ikm = await crypto.subtle.importKey("raw", shared, "HKDF", false, ["deriveBits"]);
  const okm = await crypto.subtle.deriveBits(
    { name: "HKDF", hash: "SHA-256", salt, info: new TextEncoder().encode(SEAL_INFO) },
    ikm,
    256,
  );
  const key = await crypto.subtle.importKey("raw", okm, { name: "AES-GCM" }, false, ["decrypt"]);
  return new Uint8Array(
    await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: nonce, additionalData: new TextEncoder().encode(deliveryId) },
      key,
      ct,
    ),
  );
}

export interface Envelope {
  v: number;
  hook_id: string;
  delivery_id: string;
  received_at: number;
  headers: [string, string][];
  body: string;
}

/** Reads one `hook` message from the host socket and opens it. */
export async function nextHook(
  host: Client,
  seal: SealKey,
): Promise<{ deliveryId: string; envelope: Envelope; body: Uint8Array }> {
  const m = await host.json();
  if (m.type !== "hook" || typeof m.delivery_id !== "string") {
    throw new Error(`expected hook, got ${JSON.stringify(m)}`);
  }
  const sealed = b64urlDecode(m.sealed);
  if (!sealed) throw new Error("sealed is not base64url");
  const plain = await openFromRelay(seal.privateKey, seal.raw, m.delivery_id, sealed);
  const envelope = JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(plain)) as Envelope;
  const body = b64urlDecode(envelope.body);
  if (!body) throw new Error("envelope body is not base64url");
  return { deliveryId: m.delivery_id, envelope, body };
}
