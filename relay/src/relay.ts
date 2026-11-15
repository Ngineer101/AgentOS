import { DurableObject } from "cloudflare:workers";
import {
  b64urlDecode,
  b64urlEncode,
  decodeFixed,
  hex,
  randomBytes,
  relayIdFor,
  sha256,
  signedMessage,
  verifyEd25519,
} from "./codec";
import type { Env } from "./index";

export type Role = "host" | "device" | "pair";

export const CLOSE = {
  PROTOCOL: 4400,
  REVOKED: 4401,
  FORBIDDEN: 4403,
  HOST_OFFLINE: 4404,
  AUTH_TIMEOUT: 4408,
  REPLACED: 4409,
  TOO_LARGE: 4413,
} as const;

export const AUTH_TIMEOUT_MS = 10_000;
export const MAX_FRAME = 65_600;
/** Host frames carry a prefix of at most 1 + 255 bytes on top of the payload. */
const MAX_HOST_FRAME = MAX_FRAME + 256;
/** Upper bound on simultaneously open pairing slots per host. */
export const MAX_PAIRING_SLOTS = 64;

// Storage keys. Only these are ever written:
//   host_key          the host relay public key (base64url)
//   dev:<pubkey>      one entry per allowed device relay public key
//   pair:<tokenhash>  open pairing slot -> expires_at (unix ms); deleted on use/close/expiry
const HOST_KEY = "host_key";
const DEV_PREFIX = "dev:";
const PAIR_PREFIX = "pair:";

type Stage = "pending" | "authing" | "ready" | "closed";

/** Per-socket state, kept in the WebSocket attachment so it survives hibernation. */
interface Attachment {
  role: Role;
  relayId: string;
  connId: string;
  stage: Stage;
  nonce: string;
  deadline: number;
  publicKey?: string;
}

const ROLE_TAG = (r: Role) => `role:${r}`;
const CONN_TAG = (id: string) => `conn:${id}`;

const enc = new TextEncoder();
const dec = new TextDecoder();

export class RelayObject extends DurableObject<Env> {
  /** Messages that arrive while a socket's auth is being verified (awake only). */
  private queued = new Map<WebSocket, (string | ArrayBuffer)[]>();

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    // Answer the exact keepalive string without waking the object.
    ctx.setWebSocketAutoResponse(
      new WebSocketRequestResponsePair('{"type":"ping"}', '{"type":"pong"}'),
    );
  }

  // ---------------------------------------------------------------- upgrade

  override async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const m = /^\/v1\/relay\/([0-9a-f]{32})\/(host|device|pair)$/.exec(url.pathname);
    if (!m) return new Response("not found", { status: 404 });
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
      return new Response("websocket required", { status: 426 });
    }
    const relayId = m[1]!;
    const role = m[2] as Role;

    const pair = new WebSocketPair();
    const client = pair[0];
    const server = pair[1];
    const connId = this.newConnId();
    const nonce = b64urlEncode(randomBytes(32));
    const deadline = Date.now() + AUTH_TIMEOUT_MS;

    this.ctx.acceptWebSocket(server, [ROLE_TAG(role), CONN_TAG(connId)]);
    const att: Attachment = { role, relayId, connId, stage: "pending", nonce, deadline };
    server.serializeAttachment(att);
    server.send(JSON.stringify({ type: "challenge", nonce }));
    await this.scheduleAlarm(deadline);
    log(relayId, role, "connect");

    return new Response(null, { status: 101, webSocket: client });
  }

  // ------------------------------------------------------- socket handlers

  override async webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    const att = attachment(ws);
    if (!att || att.stage === "closed") return;

    const limit = att.role === "host" && att.stage === "ready" ? MAX_HOST_FRAME : MAX_FRAME;
    if (frameSize(message) > limit) {
      this.closeSocket(ws, CLOSE.TOO_LARGE, "frame too large");
      return;
    }

    if (att.stage === "authing") {
      // Verification in progress: hold the message until it finishes.
      this.queued.get(ws)?.push(message);
      return;
    }
    if (att.stage === "pending") {
      if (Date.now() > att.deadline) {
        this.closeSocket(ws, CLOSE.AUTH_TIMEOUT, "authentication timeout");
        return;
      }
      await this.authenticate(ws, att, message);
      return;
    }
    this.handleReady(ws, att, message);
  }

  override async webSocketClose(
    ws: WebSocket,
    code: number,
    reason: string,
  ): Promise<void> {
    const att = attachment(ws);
    try {
      ws.close(code >= 3000 && code <= 4999 ? code : 1000, reason);
    } catch {
      // already closed
    }
    this.queued.delete(ws);
    if (!att || att.stage === "closed") return;
    const wasReady = att.stage === "ready";
    att.stage = "closed";
    ws.serializeAttachment(att);
    log(att.relayId, att.role, `close ${code}`);
    if (wasReady) this.afterClosed(att);
  }

  override async webSocketError(ws: WebSocket): Promise<void> {
    const att = attachment(ws);
    this.queued.delete(ws);
    if (!att || att.stage === "closed") return;
    const wasReady = att.stage === "ready";
    att.stage = "closed";
    ws.serializeAttachment(att);
    log(att.relayId, att.role, "error");
    if (wasReady) this.afterClosed(att);
  }

  /** Auth timeouts and pairing-slot expiry. */
  override async alarm(): Promise<void> {
    const now = Date.now();
    let next = Infinity;
    for (const ws of this.ctx.getWebSockets()) {
      const att = attachment(ws);
      if (!att || att.stage !== "pending") continue;
      if (att.deadline <= now) this.closeSocket(ws, CLOSE.AUTH_TIMEOUT, "authentication timeout");
      else next = Math.min(next, att.deadline);
    }
    for (const [key, expiresAt] of this.ctx.storage.kv.list<number>({ prefix: PAIR_PREFIX })) {
      if (expiresAt <= now) this.ctx.storage.kv.delete(key);
      else next = Math.min(next, expiresAt);
    }
    if (next !== Infinity) await this.ctx.storage.setAlarm(next);
  }

  // ------------------------------------------------------------ auth

  private async authenticate(
    ws: WebSocket,
    att: Attachment,
    message: string | ArrayBuffer,
  ): Promise<void> {
    if (typeof message !== "string") return this.closeSocket(ws, CLOSE.PROTOCOL, "expected auth");
    const msg = parseJson(message);
    if (!msg || msg.type !== "auth" || msg.role !== att.role) {
      return this.closeSocket(ws, CLOSE.PROTOCOL, "expected auth");
    }

    let publicKey: Uint8Array | null = null;
    let signature: Uint8Array | null = null;
    let token: Uint8Array | null = null;
    if (att.role === "pair") {
      token = b64urlDecode(msg.pairing_token);
      if (!token || token.length === 0) return this.closeSocket(ws, CLOSE.PROTOCOL, "malformed auth");
    } else {
      publicKey = decodeFixed(msg.public_key, 32);
      signature = decodeFixed(msg.signature, 64);
      if (!publicKey || !signature) return this.closeSocket(ws, CLOSE.PROTOCOL, "malformed auth");
    }

    // Device and pair sockets need a host to talk to.
    if (att.role !== "host" && !this.currentHost()) {
      return this.closeSocket(ws, CLOSE.HOST_OFFLINE, "host offline");
    }

    att.stage = "authing";
    ws.serializeAttachment(att);
    this.queued.set(ws, []);

    let ok = false;
    try {
      if (att.role === "pair") {
        ok = await this.consumePairingSlot(token!);
      } else {
        ok =
          (att.role !== "host" || (await relayIdFor(publicKey!)) === att.relayId) &&
          (await verifyEd25519(publicKey!, signature!, signedMessage(att.role, att.relayId, att.nonce)));
        if (ok && att.role === "device") {
          ok = this.ctx.storage.kv.get(DEV_PREFIX + b64urlEncode(publicKey!)) !== undefined;
        }
      }
    } catch {
      ok = false;
    }

    const queued = this.queued.get(ws) ?? [];
    this.queued.delete(ws);
    if (attachment(ws)?.stage !== "authing") return; // closed meanwhile
    if (!ok) return this.closeSocket(ws, CLOSE.FORBIDDEN, "not allowed");

    if (att.role === "host") {
      this.promoteHost(ws, att, publicKey!);
    } else {
      const host = this.currentHost();
      if (!host) return this.closeSocket(ws, CLOSE.HOST_OFFLINE, "host offline");
      if (att.role === "device") att.publicKey = b64urlEncode(publicKey!);
      att.stage = "ready";
      ws.serializeAttachment(att);
      const open: Record<string, string> = { type: "open", conn_id: att.connId, role: att.role };
      if (att.publicKey) open.public_key = att.publicKey;
      safeSend(host, JSON.stringify(open));
      safeSend(ws, JSON.stringify({ type: "ready", conn_id: att.connId }));
    }
    log(att.relayId, att.role, "ready");

    for (const m of queued) {
      const cur = attachment(ws);
      if (!cur || cur.stage !== "ready") break;
      this.handleReady(ws, cur, m);
    }
  }

  private promoteHost(ws: WebSocket, att: Attachment, publicKey: Uint8Array): void {
    // Replace any older host socket. Its device/pair connections belonged to
    // the old host session, so they are closed as "host offline" and reconnect.
    for (const old of this.ctx.getWebSockets(ROLE_TAG("host"))) {
      if (old === ws) continue;
      const oa = attachment(old);
      if (oa?.stage === "ready") {
        this.closeSocket(old, CLOSE.REPLACED, "replaced by newer host connection");
        this.closeClients();
      }
    }
    const key = b64urlEncode(publicKey);
    if (this.ctx.storage.kv.get(HOST_KEY) !== key) this.ctx.storage.kv.put(HOST_KEY, key);
    att.stage = "ready";
    att.publicKey = key;
    ws.serializeAttachment(att);
    safeSend(ws, JSON.stringify({ type: "ready", conn_id: att.connId }));
  }

  private async consumePairingSlot(token: Uint8Array): Promise<boolean> {
    const key = PAIR_PREFIX + b64urlEncode(await sha256(token));
    // Synchronous get + delete: no other event can interleave here.
    const expiresAt = this.ctx.storage.kv.get<number>(key);
    if (expiresAt === undefined) return false;
    this.ctx.storage.kv.delete(key); // single use, whatever happens next
    return expiresAt > Date.now();
  }

  // ------------------------------------------------------- after auth

  private handleReady(ws: WebSocket, att: Attachment, message: string | ArrayBuffer): void {
    if (att.role === "host") {
      if (typeof message === "string") this.handleControl(ws, message);
      else this.forwardToDevice(message);
      return;
    }
    if (typeof message === "string") {
      return this.closeSocket(ws, CLOSE.PROTOCOL, "text frame after ready");
    }
    const host = this.currentHost();
    if (!host) return this.closeSocket(ws, CLOSE.HOST_OFFLINE, "host offline");
    const id = enc.encode(att.connId);
    const out = new Uint8Array(1 + id.length + message.byteLength);
    out[0] = id.length;
    out.set(id, 1);
    out.set(new Uint8Array(message), 1 + id.length);
    safeSend(host, out);
  }

  private forwardToDevice(frame: ArrayBuffer): void {
    const bytes = new Uint8Array(frame);
    if (bytes.length < 1) return;
    const n = bytes[0]!;
    if (n === 0 || bytes.length < 1 + n) return; // malformed prefix: dropped
    if (bytes.length - 1 - n > MAX_FRAME) return; // cannot be delivered within the limit
    const connId = dec.decode(bytes.subarray(1, 1 + n));
    const target = this.findClient(connId);
    if (!target) return; // unknown conn_id: dropped
    safeSend(target, bytes.slice(1 + n));
  }

  private handleControl(host: WebSocket, text: string): void {
    const msg = parseJson(text);
    const error = (message: string) => safeSend(host, JSON.stringify({ type: "error", message }));
    if (!msg || typeof msg.type !== "string") return error("malformed control message");
    const kv = this.ctx.storage.kv;

    switch (msg.type) {
      case "ping":
        safeSend(host, JSON.stringify({ type: "pong" }));
        return;

      case "device.allow": {
        const key = canonicalKey(msg.public_key);
        if (!key) return error("device.allow: invalid public_key");
        kv.put(DEV_PREFIX + key, 1);
        return;
      }

      case "device.revoke": {
        const key = canonicalKey(msg.public_key);
        if (!key) return error("device.revoke: invalid public_key");
        kv.delete(DEV_PREFIX + key);
        this.closeDevices((k) => k === key);
        return;
      }

      case "devices.set": {
        if (!Array.isArray(msg.public_keys)) return error("devices.set: public_keys must be an array");
        const keys = new Set<string>();
        for (const k of msg.public_keys) {
          const c = canonicalKey(k);
          if (!c) return error("devices.set: invalid public key");
          keys.add(c);
        }
        for (const [k] of kv.list({ prefix: DEV_PREFIX })) {
          if (!keys.has(k.slice(DEV_PREFIX.length))) kv.delete(k);
        }
        for (const k of keys) kv.put(DEV_PREFIX + k, 1);
        // A key dropped from the set is revoked: close its sockets too.
        this.closeDevices((k) => !keys.has(k));
        return;
      }

      case "pairing.open": {
        const hash = decodeFixed(msg.token_hash, 32);
        if (!hash) return error("pairing.open: invalid token_hash");
        const expiresAt = msg.expires_at;
        if (typeof expiresAt !== "number" || !Number.isFinite(expiresAt)) {
          return error("pairing.open: invalid expires_at");
        }
        if (expiresAt <= Date.now()) return error("pairing.open: already expired");
        const key = PAIR_PREFIX + b64urlEncode(hash);
        if (kv.get(key) === undefined) {
          let count = 0;
          for (const _ of kv.list({ prefix: PAIR_PREFIX })) count++;
          if (count >= MAX_PAIRING_SLOTS) return error("pairing.open: too many open slots");
        }
        kv.put(key, expiresAt);
        void this.scheduleAlarm(expiresAt);
        return;
      }

      case "pairing.close": {
        const hash = decodeFixed(msg.token_hash, 32);
        if (!hash) return error("pairing.close: invalid token_hash");
        kv.delete(PAIR_PREFIX + b64urlEncode(hash));
        return;
      }

      case "close": {
        if (typeof msg.conn_id !== "string") return error("close: invalid conn_id");
        let code = 4000;
        if (msg.code !== undefined) {
          if (!Number.isInteger(msg.code) || (msg.code as number) < 4000 || (msg.code as number) > 4999) {
            return error("close: code must be 4000-4999");
          }
          code = msg.code as number;
        }
        const target = this.findClient(msg.conn_id);
        if (target) this.closeSocket(target, code, "closed by host");
        return;
      }

      default:
        return error(`unknown control message: ${msg.type}`);
    }
  }

  // ------------------------------------------------------------ helpers

  private currentHost(): WebSocket | null {
    for (const ws of this.ctx.getWebSockets(ROLE_TAG("host"))) {
      if (attachment(ws)?.stage === "ready") return ws;
    }
    return null;
  }

  private findClient(connId: string): WebSocket | null {
    for (const ws of this.ctx.getWebSockets(CONN_TAG(connId))) {
      const att = attachment(ws);
      if (att && att.role !== "host" && att.stage === "ready") return ws;
    }
    return null;
  }

  private closeDevices(match: (publicKey: string) => boolean): void {
    for (const ws of this.ctx.getWebSockets(ROLE_TAG("device"))) {
      const att = attachment(ws);
      if (att?.stage === "ready" && att.publicKey && match(att.publicKey)) {
        this.closeSocket(ws, CLOSE.REVOKED, "device revoked");
      }
    }
  }

  /** Closes every device and pair socket (any stage) as "host offline". */
  private closeClients(): void {
    for (const role of ["device", "pair"] as const) {
      for (const ws of this.ctx.getWebSockets(ROLE_TAG(role))) {
        const att = attachment(ws);
        if (att && att.stage !== "closed") {
          att.stage = "closed";
          ws.serializeAttachment(att);
          this.queued.delete(ws);
          try {
            ws.close(CLOSE.HOST_OFFLINE, "host offline");
          } catch {
            // ignore
          }
          log(att.relayId, att.role, `close ${CLOSE.HOST_OFFLINE}`);
        }
      }
    }
  }

  /** Relay-initiated close. Marks the socket closed first so handlers ignore it. */
  private closeSocket(ws: WebSocket, code: number, reason: string): void {
    const att = attachment(ws);
    this.queued.delete(ws);
    if (att) {
      if (att.stage === "closed") return;
      const wasReady = att.stage === "ready";
      att.stage = "closed";
      ws.serializeAttachment(att);
      log(att.relayId, att.role, `close ${code}`);
      if (wasReady) this.afterClosed(att);
    }
    try {
      ws.close(code, reason);
    } catch {
      // ignore
    }
  }

  /** Bookkeeping after a ready socket goes away. */
  private afterClosed(att: Attachment): void {
    if (att.role === "host") {
      // Only the current host going away takes the devices down; a replaced
      // host is closed after its successor is already in charge.
      if (!this.currentHost()) this.closeClients();
      return;
    }
    const host = this.currentHost();
    if (host) safeSend(host, JSON.stringify({ type: "closed", conn_id: att.connId }));
  }

  private newConnId(): string {
    for (;;) {
      const id = hex(randomBytes(8));
      if (this.ctx.getWebSockets(CONN_TAG(id)).length === 0) return id;
    }
  }

  private async scheduleAlarm(at: number): Promise<void> {
    const current = await this.ctx.storage.getAlarm();
    if (current === null || at < current) await this.ctx.storage.setAlarm(at);
  }
}

// ---------------------------------------------------------------- utils

function attachment(ws: WebSocket): Attachment | null {
  try {
    return (ws.deserializeAttachment() as Attachment | null) ?? null;
  } catch {
    return null;
  }
}

function frameSize(message: string | ArrayBuffer): number {
  if (typeof message !== "string") return message.byteLength;
  // UTF-8 length is between length and 3 * length.
  if (message.length * 3 <= MAX_FRAME) return message.length;
  return enc.encode(message).byteLength;
}

function parseJson(text: string): Record<string, unknown> | null {
  try {
    const v = JSON.parse(text);
    return v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

function canonicalKey(v: unknown): string | null {
  const b = decodeFixed(v, 32);
  return b ? b64urlEncode(b) : null;
}

function safeSend(ws: WebSocket, data: string | ArrayBuffer | Uint8Array): void {
  try {
    ws.send(data);
  } catch {
    // socket is going away; its close handler does the bookkeeping
  }
}

/** Connection lifecycle only. Never payloads, keys or tokens. */
function log(relayId: string, role: Role, event: string): void {
  console.log(`relay=${relayId} role=${role} ${event}`);
}
