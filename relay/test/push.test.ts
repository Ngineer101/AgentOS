import { exports } from "cloudflare:workers";
import { describe, expect, it } from "vitest";
import { b64urlDecode, b64urlEncode, decodeFixed, relayIdFor, sha256, verifyEd25519 } from "../src/codec";
import { GENERIC_BODY, GENERIC_TITLE, SEND_RATE_LIMIT, register, sendMessage } from "../src/push";
import { BASE, type Identity, newIdentity, relayId } from "./helpers";

// The APNs stand-in is in vitest.config.ts: tokens starting `dead` are
// Unregistered, `5b` work only in the sandbox, `ec` echo the request back.

const TOPIC = "dev.agenthost.ios";

async function registerToken(
  rid: string,
  token: string,
  extra: Record<string, unknown> = {},
): Promise<Response> {
  return exports.default.fetch(
    new Request(`${BASE}/v1/push/register`, {
      method: "POST",
      body: JSON.stringify({ token, topic: TOPIC, environment: "production", relay_id: rid, ...extra }),
    }),
  );
}

async function handleFor(rid: string, token: string): Promise<string> {
  const r = await registerToken(rid, token);
  expect(r.status).toBe(200);
  const { handle } = (await r.json()) as { handle: string };
  expect(handle).not.toContain(token);
  return handle;
}

async function send(
  host: Identity,
  body: Record<string, unknown>,
  opts: { rid?: string; time?: number; tamper?: boolean } = {},
): Promise<Response> {
  const rid = opts.rid ?? (await relayId(host));
  const text = JSON.stringify(body);
  const time = String(opts.time ?? Date.now());
  const sig = await crypto.subtle.sign(
    "Ed25519",
    host.privateKey,
    sendMessage(rid, time, await sha256(new TextEncoder().encode(text))),
  );
  return exports.default.fetch(
    new Request(`${BASE}/v1/push/${rid}/send`, {
      method: "POST",
      headers: {
        "x-agenthost-key": host.publicKey,
        "x-agenthost-time": time,
        "x-agenthost-signature": b64urlEncode(new Uint8Array(sig)),
      },
      body: opts.tamper ? text.replace("}", ',"x":1}') : text,
    }),
  );
}

const SEALED = b64urlEncode(new Uint8Array(60).fill(7));

describe("push", () => {
  it("sends a host's notification to the phone that registered for it", async () => {
    const host = await newIdentity();
    const rid = await relayId(host);
    const handle = await handleFor(rid, "a0".repeat(32));
    const r = await send(host, { handle, sealed: SEALED, collapse_id: "apr_1" });
    expect(r.status).toBe(200);
    expect(await r.json()).toEqual({ environment: "production" });
  });

  it("shows Apple only the generic alert and the sealed content", async () => {
    const host = await newIdentity();
    const handle = await handleFor(await relayId(host), "ec".repeat(32));
    const r = await send(host, { handle, sealed: SEALED, collapse_id: "turn-ses_1" });
    expect(r.status).toBe(400);
    const seen = JSON.parse(((await r.json()) as { reason: string }).reason);
    expect(seen).toEqual({
      env: "production",
      collapse: "turn-ses_1",
      payload: {
        aps: { alert: { title: GENERIC_TITLE, body: GENERIC_BODY }, sound: "default", "mutable-content": 1 },
        v: 1,
        e: SEALED,
      },
    });
  });

  it("tries the other environment when Apple says BadDeviceToken", async () => {
    const host = await newIdentity();
    const handle = await handleFor(await relayId(host), "5b".repeat(32));
    const r = await send(host, { handle, sealed: SEALED });
    expect(r.status).toBe(200);
    expect(await r.json()).toEqual({ environment: "sandbox" });
  });

  it("passes on Unregistered, so the host forgets the phone", async () => {
    const host = await newIdentity();
    const handle = await handleFor(await relayId(host), "dead".repeat(16));
    const r = await send(host, { handle, sealed: SEALED });
    expect(r.status).toBe(410);
    expect(await r.json()).toEqual({ reason: "Unregistered" });
  });

  it("lets only the host a handle names use it", async () => {
    const a = await newIdentity();
    const b = await newIdentity();
    const handle = await handleFor(await relayId(a), "a1".repeat(32));
    const r = await send(b, { handle, sealed: SEALED });
    expect(r.status).toBe(400);
    expect(await r.json()).toEqual({ reason: "BadHandle" });
    // A handle that has been changed doesn't open.
    const raw = b64urlDecode(handle)!;
    raw[20]! ^= 1;
    const bad = await send(a, { handle: b64urlEncode(raw), sealed: SEALED });
    expect(((await bad.json()) as { reason: string }).reason).toBe("BadHandle");
  });

  it("refuses requests the host didn't sign, or signed long ago", async () => {
    const host = await newIdentity();
    const other = await newIdentity();
    const rid = await relayId(host);
    const handle = await handleFor(rid, "a2".repeat(32));
    const body = { handle, sealed: SEALED };
    for (const r of [
      await send(host, body, { tamper: true }),
      await send(host, body, { time: Date.now() - 10 * 60 * 1000 }),
      // Someone else's key for this relay id.
      await send(other, body, { rid }),
    ]) {
      expect(r.status).toBe(401);
      expect(((await r.json()) as { reason: string }).reason).toBe("BadSignature");
    }
    const unsigned = await exports.default.fetch(
      new Request(`${BASE}/v1/push/${rid}/send`, { method: "POST", body: JSON.stringify(body) }),
    );
    expect(unsigned.status).toBe(401);
  });

  it("checks what it is given", async () => {
    const host = await newIdentity();
    const rid = await relayId(host);
    expect((await registerToken(rid, "not hex")).status).toBe(400);
    expect((await registerToken(rid, "a0".repeat(32), { environment: "staging" })).status).toBe(400);
    expect((await registerToken("nope", "a0".repeat(32))).status).toBe(400);
    // Another app (another team's bundle id) needs that team's key.
    expect((await registerToken(rid, "a0".repeat(32), { topic: "com.example.other" })).status).toBe(403);
    const handle = await handleFor(rid, "a3".repeat(32));
    for (const body of [
      { handle, sealed: "x".repeat(4000) },
      { handle, sealed: "not base64url!" },
      { handle, sealed: SEALED, collapse_id: "c".repeat(65) },
    ]) {
      expect((await send(host, body)).status).toBe(400);
    }
    const get = await exports.default.fetch(new Request(`${BASE}/v1/push/register`));
    expect(get.status).toBe(405);
  });

  it("refuses an oversized body as it streams, and the headers decide first", async () => {
    // H4: a sender that leaves out content-length and keeps writing must not
    // get the Worker (or one host's Durable Object) to hold the whole body
    // first and measure it after. The stream is cancelled at the limit, so
    // only a bounded prefix of what was offered is ever read.
    const CHUNK = 65536;
    const CHUNKS = 64; // 4 MB offered to routes whose limit is 4-8 KB
    let written = 0;
    const flood = (): ReadableStream =>
      new ReadableStream({
        pull(controller) {
          if (written >= CHUNKS * CHUNK) return controller.close();
          written += CHUNK;
          controller.enqueue(new Uint8Array(CHUNK).fill(65));
        },
      });
    const post = (url: string, headers: Record<string, string> = {}): Promise<Response> =>
      exports.default.fetch(
        new Request(url, { method: "POST", headers, body: flood(), duplex: "half" } as RequestInit),
      );

    const host = await newIdentity();
    const rid = await relayId(host);
    const time = String(Date.now());
    const sig = b64urlEncode(
      new Uint8Array(
        await crypto.subtle.sign(
          "Ed25519",
          host.privateKey,
          sendMessage(rid, time, await sha256(new Uint8Array(0))),
        ),
      ),
    );
    written = 0;
    expect(
      (
        await post(`${BASE}/v1/push/${rid}/send`, {
          "x-agenthost-key": host.publicKey,
          "x-agenthost-time": time,
          "x-agenthost-signature": sig,
        })
      ).status,
    ).toBe(413);
    expect(written).toBeLessThan(CHUNKS * CHUNK);

    // Registration is open to anyone, so it is the one that most needs this.
    written = 0;
    expect((await post(`${BASE}/v1/push/register`)).status).toBe(400);
    expect(written).toBeLessThan(CHUNKS * CHUNK);

    // Headers that aren't even the right shape settle it before the body is
    // read at all: 401, not the 413 a drained body would have produced.
    written = 0;
    expect(
      (
        await post(`${BASE}/v1/push/${rid}/send`, {
          "x-agenthost-key": "nonsense",
          "x-agenthost-time": "x",
          "x-agenthost-signature": "no",
        })
      ).status,
    ).toBe(401);
  });

  it("verifies a host's signature from Rust", async () => {
    // crates/proto/src/tunnel.rs, push_signature_vector.
    const key = decodeFixed("6kpsY-KcUgq-9VB7Ey7F-ZVHdq6-vnuSQh7qaRRG0iw", 32)!;
    const sig = decodeFixed(
      "z_0o6k8VRDieNQGynuBypVHBdzIZt2wryiaACSMMm_nMsmdSQKY-RRUDxgh2dnUsHnV2aq0tdeEYB_iiPp7FDQ",
      64,
    )!;
    const rid = await relayIdFor(key);
    expect(rid).toBe("fe812c12f3ab4ce6ac5db69ac352f906");
    const body = new TextEncoder().encode('{"handle":"h","sealed":"s"}');
    expect(await verifyEd25519(key, sig, sendMessage(rid, "1700000000000", await sha256(body)))).toBe(true);
    expect(await verifyEd25519(key, sig, sendMessage(rid, "1700000000001", await sha256(body)))).toBe(false);
  });

  it("says which app it sends for, and when its key is broken", async () => {
    const env = { APNS_KEY: "-----BEGIN PRIVATE KEY-----\nnope\n-----END PRIVATE KEY-----", APNS_KEY_ID: "KEY1234567", APNS_TEAM_ID: "TEAM123456", APNS_TOPIC: TOPIC };
    const body = (topic: string) =>
      JSON.stringify({ token: "a0".repeat(32), topic, environment: "sandbox", relay_id: "0".repeat(32) });
    const other = await registerToken("0".repeat(32), "a0".repeat(32), { topic: "com.example.other" });
    expect(await other.json()).toEqual({
      error: "this relay sends notifications for dev.agenthost.ios only, not com.example.other",
    });
    const broken = await register(new Request(`${BASE}/v1/push/register`, { method: "POST", body: body(TOPIC) }), env);
    expect(broken.status).toBe(503);
    expect(((await broken.json()) as { error: string }).error).toContain("APNS_KEY");
  });

  it("answers 404 where nobody set up an APNs key", async () => {
    const r = await register(
      new Request(`${BASE}/v1/push/register`, { method: "POST", body: "{}" }),
      {},
    );
    expect(r.status).toBe(404);
  });

  it("limits how many notifications one host sends", async () => {
    const host = await newIdentity();
    const handle = await handleFor(await relayId(host), "a4".repeat(32));
    const results = await Promise.all(
      Array.from({ length: SEND_RATE_LIMIT + 1 }, () => send(host, { handle, sealed: SEALED })),
    );
    const statuses = results.map((r) => r.status).sort();
    expect(statuses.filter((s) => s === 200).length).toBe(SEND_RATE_LIMIT);
    expect(statuses.filter((s) => s === 429).length).toBe(1);
  });
});
