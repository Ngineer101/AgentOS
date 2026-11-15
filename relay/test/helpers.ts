import { exports } from "cloudflare:workers";
import { b64urlEncode, relayIdFor, sha256, signedMessage } from "../src/codec";

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

export async function connectHost(host: Identity, devices: Identity[] = []) {
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
