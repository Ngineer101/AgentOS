import { DurableObject } from "cloudflare:workers";
import {
  b64urlDecode,
  b64urlEncode,
  decodeFixed,
  hex,
  randomBytes,
  readLimited,
  relayIdFor,
  sha256,
  signedMessage,
  verifyEd25519,
} from "./codec";
import type { Env } from "./index";
import { PushSender } from "./push";
import { sealToHost } from "./seal";

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

/** Webhook ingress limits. */
export const MAX_HOOK_BODY = 262_144;
export const MAX_HOOKS = 256;
export const MAX_QUEUED_HOOKS = 200;
export const HOOK_RATE_LIMIT = 60;
export const HOOK_RATE_WINDOW_MS = 60_000;
export const HOOK_TTL_MS = 24 * 60 * 60 * 1000;
/** Request headers never put into a webhook envelope (besides any `cf-*`). */
const DROPPED_HEADERS = new Set(["cookie", "x-forwarded-for", "x-real-ip", "true-client-ip"]);

// Storage keys. Only these are ever written:
//   host_key          the host relay public key (base64url)
//   dev:<pubkey>      one entry per allowed device relay public key
//   pair:<tokenhash>  open pairing slot -> expires_at (unix ms); deleted on use/close/expiry
//   hook_key          the host's X25519 seal key for webhook deliveries (base64url)
//   hook:<hash>       one entry per registered hook: base64url(sha256(hook_id))
//   hookq:<id>        a sealed webhook delivery -> {sealed, at}; deleted on ack or after 24 h
const HOST_KEY = "host_key";
const DEV_PREFIX = "dev:";
const PAIR_PREFIX = "pair:";
const HOOK_KEY = "hook_key";
const HOOK_PREFIX = "hook:";
const HOOKQ_PREFIX = "hookq:";

interface QueuedHook {
  sealed: string;
  at: number;
}

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
  /** Number of `hookq:` entries; loaded lazily, then kept in step (awake only). */
  private hookQueueSize: number | null = null;
  /** Recent request times per hook hash, for the rate limit (awake only). */
  private hookHits = new Map<string, number[]>();
  /** Last millisecond used in a delivery_id, so ids sort in arrival order. */
  private lastDeliveryMs = 0;
  /** Push notifications this host sends through the relay (awake only). */
  private push: PushSender;

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.push = new PushSender(env);
    // Answer the exact keepalive string without waking the object.
    ctx.setWebSocketAutoResponse(
      new WebSocketRequestResponsePair('{"type":"ping"}', '{"type":"pong"}'),
    );
  }

  // ---------------------------------------------------------------- upgrade

  override async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const hook = /^\/v1\/hook\/([0-9a-f]{32})\/([A-Za-z0-9_-]{22,64})$/.exec(url.pathname);
    if (hook) return this.ingestHook(request, hook[1]!, hook[2]!);
    const send = /^\/v1\/push\/([0-9a-f]{32})\/send$/.exec(url.pathname);
    if (send) return this.push.send(request, send[1]!);
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

  /** Auth timeouts, pairing-slot expiry and expiry of undelivered webhooks. */
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
    // Queue keys sort by arrival, so only a prefix of them can have expired.
    for (const [key, entry] of this.ctx.storage.kv.list<QueuedHook>({ prefix: HOOKQ_PREFIX })) {
      if (entry.at + HOOK_TTL_MS <= now) {
        this.deleteQueued(key);
      } else {
        next = Math.min(next, entry.at + HOOK_TTL_MS);
        break;
      }
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
    // Undelivered (or unacknowledged) webhooks, oldest first.
    const cutoff = Date.now() - HOOK_TTL_MS;
    for (const [k, entry] of this.ctx.storage.kv.list<QueuedHook>({ prefix: HOOKQ_PREFIX })) {
      if (entry.at <= cutoff) {
        this.deleteQueued(k);
        continue;
      }
      safeSend(ws, hookFrame(k.slice(HOOKQ_PREFIX.length), entry.sealed));
    }
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
        // Echo an id so the host can tell when everything it sent before
        // this ping has been handled (the auto-response has none).
        safeSend(
          host,
          JSON.stringify(Number.isSafeInteger(msg.id) ? { type: "pong", id: msg.id } : { type: "pong" }),
        );
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

      case "hooks.set": {
        const sealKey = canonicalKey(msg.seal_key);
        if (!sealKey) return error("hooks.set: invalid seal_key");
        if (!Array.isArray(msg.hooks)) return error("hooks.set: hooks must be an array");
        if (msg.hooks.length > MAX_HOOKS) return error(`hooks.set: at most ${MAX_HOOKS} hooks`);
        const hashes = new Set<string>();
        for (const h of msg.hooks) {
          const c = canonicalKey(h);
          if (!c) return error("hooks.set: invalid hook hash");
          hashes.add(c);
        }
        if (kv.get(HOOK_KEY) !== sealKey) kv.put(HOOK_KEY, sealKey);
        for (const [k] of kv.list({ prefix: HOOK_PREFIX })) {
          if (!hashes.has(k.slice(HOOK_PREFIX.length))) kv.delete(k);
        }
        for (const h of hashes) {
          if (kv.get(HOOK_PREFIX + h) === undefined) kv.put(HOOK_PREFIX + h, 1);
        }
        for (const h of this.hookHits.keys()) if (!hashes.has(h)) this.hookHits.delete(h);
        return;
      }

      case "hook.ack": {
        if (typeof msg.delivery_id !== "string") return error("hook.ack: invalid delivery_id");
        this.deleteQueued(HOOKQ_PREFIX + msg.delivery_id); // unknown id: nothing to do
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

  // ------------------------------------------------------------ webhooks

  /**
   * Webhook ingress. The plaintext request exists only in memory here: it is
   * sealed to the host's seal key at once, and only the sealed form is stored
   * and forwarded. Answers without waiting for the host.
   */
  private async ingestHook(request: Request, relayId: string, hookId: string): Promise<Response> {
    const done = async (status: number, body: Record<string, unknown>, headers?: HeadersInit) => {
      log(relayId, "hook", `${status === 202 ? "accepted" : "rejected"} ${status}`);
      // Consume (at most the limit of) an unread body before replying; the
      // runtime raises an error if the request stream is left unread.
      if (request.body && !request.bodyUsed) await readLimited(request, MAX_HOOK_BODY).catch(() => null);
      return jsonResponse(status, body, headers);
    };
    if (request.method !== "POST") {
      return done(405, { error: "method not allowed" }, { allow: "POST" });
    }
    const kv = this.ctx.storage.kv;
    const hash = b64urlEncode(await sha256(enc.encode(hookId)));
    if (!this.hookRegistered(hash)) return done(404, { error: "unknown hook" });

    const declared = request.headers.get("content-length");
    if (declared !== null && !(Number(declared) <= MAX_HOOK_BODY)) {
      return done(413, { error: "payload too large" });
    }
    if (!this.hookAllowed(hash)) {
      return done(429, { error: "rate limited" }, { "retry-after": "60" });
    }
    if (this.queuedHookCount() >= MAX_QUEUED_HOOKS) {
      return done(503, { error: "queue full" }, { "retry-after": "60" });
    }

    let body: Uint8Array | null;
    try {
      body = await readLimited(request, MAX_HOOK_BODY);
    } catch {
      return done(400, { error: "could not read body" });
    }
    if (!body) return done(413, { error: "payload too large" });

    const receivedAt = Date.now();
    const ms = Math.max(receivedAt, this.lastDeliveryMs + 1);
    this.lastDeliveryMs = ms;
    const deliveryId = `${String(ms).padStart(13, "0")}-${hex(randomBytes(8))}`;

    const headers: [string, string][] = [];
    for (const [name, value] of request.headers) {
      const n = name.toLowerCase();
      if (n.startsWith("cf-") || DROPPED_HEADERS.has(n)) continue;
      headers.push([n, value]);
    }
    const envelope = enc.encode(
      JSON.stringify({
        v: 1,
        hook_id: hookId,
        delivery_id: deliveryId,
        received_at: receivedAt,
        headers,
        body: b64urlEncode(body),
      }),
    );

    // The seal key and hook set may have changed while we awaited the body;
    // everything from here to the reply is synchronous.
    let sealed: string;
    for (;;) {
      const sealKey = decodeFixed(kv.get(HOOK_KEY), 32);
      if (!sealKey) return done(404, { error: "unknown hook" });
      try {
        sealed = b64urlEncode(await sealToHost(sealKey, deliveryId, envelope));
      } catch {
        return done(500, { error: "internal error" });
      }
      if (kv.get(HOOK_KEY) === b64urlEncode(sealKey)) break;
    }
    if (!this.hookRegistered(hash)) return done(404, { error: "unknown hook" });
    if (this.queuedHookCount() >= MAX_QUEUED_HOOKS) {
      return done(503, { error: "queue full" }, { "retry-after": "60" });
    }

    const entry: QueuedHook = { sealed, at: receivedAt };
    kv.put(HOOKQ_PREFIX + deliveryId, entry); // before the 202: at-least-once
    if (this.hookQueueSize !== null) this.hookQueueSize++;
    const host = this.currentHost();
    if (host) safeSend(host, hookFrame(deliveryId, sealed));
    await this.scheduleAlarm(receivedAt + HOOK_TTL_MS);
    return done(202, { delivery_id: deliveryId });
  }

  private hookRegistered(hash: string): boolean {
    const kv = this.ctx.storage.kv;
    return kv.get(HOOK_KEY) !== undefined && kv.get(HOOK_PREFIX + hash) !== undefined;
  }

  /** Sliding-window rate limit per hook; best effort (resets on eviction). */
  private hookAllowed(hash: string): boolean {
    const now = Date.now();
    const hits = (this.hookHits.get(hash) ?? []).filter((t) => t > now - HOOK_RATE_WINDOW_MS);
    if (hits.length >= HOOK_RATE_LIMIT) {
      this.hookHits.set(hash, hits);
      return false;
    }
    hits.push(now);
    this.hookHits.set(hash, hits);
    return true;
  }

  private queuedHookCount(): number {
    if (this.hookQueueSize === null) {
      let n = 0;
      for (const _ of this.ctx.storage.kv.list({ prefix: HOOKQ_PREFIX })) n++;
      this.hookQueueSize = n;
    }
    return this.hookQueueSize;
  }

  private deleteQueued(key: string): void {
    if (this.ctx.storage.kv.delete(key) && this.hookQueueSize !== null) {
      this.hookQueueSize = Math.max(0, this.hookQueueSize - 1);
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

function hookFrame(deliveryId: string, sealed: string): string {
  return JSON.stringify({ type: "hook", delivery_id: deliveryId, sealed });
}

export function jsonResponse(status: number, body: unknown, headers?: HeadersInit): Response {
  const h = new Headers(headers);
  h.set("content-type", "application/json");
  return new Response(JSON.stringify(body), { status, headers: h });
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

/** Connection and webhook lifecycle only. Never payloads, headers, hook ids, keys or tokens. */
function log(relayId: string, role: Role | "hook", event: string): void {
  console.log(`relay=${relayId} role=${role} ${event}`);
}
