import { env, exports } from "cloudflare:workers";
import { runDurableObjectAlarm, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { b64urlDecode, b64urlEncode } from "../src/codec";
import { HOOK_RATE_LIMIT, MAX_FRAME, MAX_HOOK_BODY, MAX_HOOKS, MAX_QUEUED_HOOKS } from "../src/relay";
import { sealToHost } from "../src/seal";
import {
  BASE,
  type Client,
  type Identity,
  type SealKey,
  authHost,
  connectHost,
  hookHash,
  newHookId,
  newIdentity,
  newSealKey,
  nextHook,
  openFromRelay,
  ping,
  postHook,
  setHooks,
} from "./helpers";

const DELIVERY_ID = /^\d{13}-[0-9a-f]{16}$/;

interface Setup {
  host: Identity;
  seal: SealKey;
  rid: string;
  c: Client;
  hookIds: string[];
}

/** A host with `n` registered hooks, still connected. */
async function withHooks(n = 1): Promise<Setup> {
  const host = await newIdentity();
  const seal = await newSealKey();
  const { c, rid } = await connectHost(host);
  const hookIds = Array.from({ length: n }, newHookId);
  await setHooks(c, seal, hookIds);
  return { host, seal, rid, c, hookIds };
}

async function disconnect(c: Client): Promise<void> {
  c.ws.close(1000, "bye");
  await c.closed;
}

/** Hooks registered, host gone again: deliveries queue up. */
async function offlineWithHooks(n = 1): Promise<Setup> {
  const s = await withHooks(n);
  await disconnect(s.c);
  return s;
}

async function accepted(res: Response): Promise<string> {
  expect(res.status).toBe(202);
  const body = (await res.json()) as { delivery_id: string };
  expect(body.delivery_id).toMatch(DELIVERY_ID);
  return body.delivery_id;
}

function storageOf(rid: string) {
  return env.RELAY.get(env.RELAY.idFromName(rid));
}

async function queuedIds(rid: string): Promise<string[]> {
  return runInDurableObject(storageOf(rid), (_i, state) =>
    [...state.storage.kv.list({ prefix: "hookq:" })].map(([k]) => k.slice("hookq:".length)),
  );
}

describe("sealing", () => {
  it("round-trips and binds the delivery_id", async () => {
    const seal = await newSealKey();
    const plain = new TextEncoder().encode('{"v":1,"hello":"world"}');
    const id = "0001700000000000-00112233aabbccdd";
    const sealed = await sealToHost(seal.raw, id, plain);
    expect(sealed.length).toBe(32 + 12 + plain.length + 16);
    expect(await openFromRelay(seal.privateKey, seal.raw, id, sealed)).toEqual(plain);
    await expect(openFromRelay(seal.privateKey, seal.raw, id.replace("0", "1"), sealed)).rejects.toThrow();
    const tampered = sealed.slice();
    tampered[tampered.length - 1]! ^= 1;
    await expect(openFromRelay(seal.privateKey, seal.raw, id, tampered)).rejects.toThrow();
    const other = await newSealKey();
    await expect(openFromRelay(other.privateKey, other.raw, id, sealed)).rejects.toThrow();
  });

  it("uses a fresh ephemeral key and nonce every time", async () => {
    const seal = await newSealKey();
    const a = await sealToHost(seal.raw, "x", new Uint8Array([1]));
    const b = await sealToHost(seal.raw, "x", new Uint8Array([1]));
    expect(b64urlEncode(a.slice(0, 44))).not.toBe(b64urlEncode(b.slice(0, 44)));
  });
});

describe("hook ingress http", () => {
  const rid = "a".repeat(32);
  const hookId = "A".repeat(32);

  it("405s anything but POST", async () => {
    for (const method of ["GET", "PUT", "DELETE"]) {
      const res = await exports.default.fetch(`${BASE}/v1/hook/${rid}/${hookId}`, { method });
      expect(res.status).toBe(405);
      expect(res.headers.get("allow")).toBe("POST");
    }
  });

  it("400s a bad relay_id", async () => {
    for (const id of ["abc", "A".repeat(32), "0".repeat(33)]) {
      expect((await postHook(id, hookId)).status).toBe(400);
    }
  });

  it("404s a hook_id of the wrong shape", async () => {
    for (const id of ["short", "a".repeat(21), "a".repeat(65), "a".repeat(30) + "!!", "a".repeat(30) + "%2F"]) {
      expect((await postHook(rid, id)).status).toBe(404);
    }
    expect((await exports.default.fetch(`${BASE}/v1/hook/${rid}`, { method: "POST" })).status).toBe(404);
  });

  it("404s an unknown hook, with or without a registered seal key", async () => {
    const res = await postHook(rid, hookId);
    expect(res.status).toBe(404);
    expect(await res.json()).toEqual({ error: "unknown hook" });

    const s = await withHooks(1);
    const other = await postHook(s.rid, newHookId());
    expect(other.status).toBe(404);
    expect(await other.json()).toEqual({ error: "unknown hook" });
    expect(await s.c.silent()).toBe(true);
  });

  it("413s by Content-Length and by actual bytes; accepts exactly the limit", async () => {
    const s = await withHooks(1);
    const [hookId] = s.hookIds as [string];

    const big = await postHook(s.rid, hookId, new Uint8Array(MAX_HOOK_BODY + 1));
    expect(big.status).toBe(413);

    // No Content-Length: a stream that turns out too long.
    const chunk = new Uint8Array(65_536);
    let sent = 0;
    const stream = new ReadableStream<Uint8Array>({
      pull(ctrl) {
        if (sent > MAX_HOOK_BODY) return ctrl.close();
        sent += chunk.length;
        ctrl.enqueue(chunk);
      },
    });
    const streamed = await postHook(s.rid, hookId, stream);
    expect(streamed.status).toBe(413);
    expect(await s.c.silent()).toBe(true);

    // Exactly the limit is accepted, and the (large) frame reaches the host.
    const body = new Uint8Array(MAX_HOOK_BODY);
    for (let i = 0; i < body.length; i++) body[i] = i & 0xff;
    const id = await accepted(await postHook(s.rid, hookId, body));
    const got = await nextHook(s.c, s.seal);
    expect(got.deliveryId).toBe(id);
    expect(got.body).toEqual(body);
    expect(got.envelope.body.length).toBeGreaterThan(MAX_FRAME);
  });
});

describe("hook delivery", () => {
  it("delivers a sealed envelope to the connected host", async () => {
    const s = await withHooks(1);
    const [hookId] = s.hookIds as [string];
    const body = new Uint8Array([0xff, 0x00, 0x80, 0xfe, 0x7b, 0x22, 0xc3, 0x28]);
    const before = Date.now();
    const res = await postHook(s.rid, hookId, body, {
      "Content-Type": "application/json",
      "Sentry-Hook-Resource": "issue",
      "Sentry-Hook-Signature": "abc123",
      "X-GitHub-Event": "push",
      Authorization: "Bearer t",
      "User-Agent": "Sentry",
      "X-Custom-Thing": "Mixed Case Value",
      "CF-Connecting-IP": "203.0.113.1",
      "cf-ray": "abc",
      "CF-IPCountry": "ZA",
      Cookie: "session=secret",
      "X-Forwarded-For": "203.0.113.1",
      "X-Real-IP": "203.0.113.1",
      "True-Client-IP": "203.0.113.1",
    });
    expect(res.headers.get("content-type")).toBe("application/json");
    const id = await accepted(res);
    expect(Number(id.slice(0, 13))).toBeGreaterThanOrEqual(before - 1000);

    const got = await nextHook(s.c, s.seal);
    expect(got.deliveryId).toBe(id);
    expect(got.body).toEqual(body);
    const env = got.envelope;
    expect(env.v).toBe(1);
    expect(env.hook_id).toBe(hookId);
    expect(env.delivery_id).toBe(id);
    expect(typeof env.received_at).toBe("number");
    expect(Math.abs(env.received_at - Number(id.slice(0, 13)))).toBeLessThan(1000);
    const headers = Object.fromEntries(env.headers);
    expect(headers).toMatchObject({
      "content-type": "application/json",
      "sentry-hook-resource": "issue",
      "sentry-hook-signature": "abc123",
      "x-github-event": "push",
      authorization: "Bearer t",
      "user-agent": "Sentry",
      "x-custom-thing": "Mixed Case Value",
    });
    for (const [name] of env.headers) {
      expect(name).toBe(name.toLowerCase());
      expect(name.startsWith("cf-")).toBe(false);
      expect(["cookie", "x-forwarded-for", "x-real-ip", "true-client-ip"]).not.toContain(name);
    }

    // Storage holds the sealed form only, until the host acks.
    const stored = await runInDurableObject(storageOf(s.rid), (_i, state) =>
      state.storage.kv.get<{ sealed: string; at: number }>(`hookq:${id}`),
    );
    expect(stored?.at).toBe(env.received_at);
    const sealed = b64urlDecode(stored?.sealed)!;
    expect(await openFromRelay(s.seal.privateKey, s.seal.raw, id, sealed)).toBeTruthy();
    expect(JSON.stringify(stored)).not.toContain("secret");
    expect(JSON.stringify(stored)).not.toContain(hookId);
  });

  it("queues while the host is offline and delivers in order on connect", async () => {
    const s = await offlineWithHooks(2);
    const [a, b] = s.hookIds as [string, string];
    const ids: string[] = [];
    for (const [i, h] of [a, b, a, b, a].entries()) {
      ids.push(await accepted(await postHook(s.rid, h, `{"n":${i}}`)));
    }
    expect([...ids].sort()).toEqual(ids);
    expect(await queuedIds(s.rid)).toEqual(ids);

    const { c } = await authHost(s.host);
    for (const [i, id] of ids.entries()) {
      const got = await nextHook(c, s.seal);
      expect(got.deliveryId).toBe(id);
      expect(new TextDecoder().decode(got.body)).toBe(`{"n":${i}}`);
    }
    await ping(c);
  });

  it("an acked delivery is deleted and not sent again", async () => {
    const s = await withHooks(1);
    const id = await accepted(await postHook(s.rid, s.hookIds[0]!));
    expect((await nextHook(s.c, s.seal)).deliveryId).toBe(id);
    s.c.send({ type: "hook.ack", delivery_id: id });
    await ping(s.c);
    expect(await queuedIds(s.rid)).toEqual([]);
    await disconnect(s.c);

    const { c } = await authHost(s.host);
    await ping(c); // the first thing it hears is the pong, not a hook
    expect(await c.silent()).toBe(true);
  });

  it("an unacked delivery is sent again when the host reconnects", async () => {
    const s = await withHooks(1);
    const id = await accepted(await postHook(s.rid, s.hookIds[0]!, "once"));
    expect((await nextHook(s.c, s.seal)).deliveryId).toBe(id);
    await disconnect(s.c);

    const { c } = await authHost(s.host);
    const again = await nextHook(c, s.seal);
    expect(again.deliveryId).toBe(id);
    expect(new TextDecoder().decode(again.body)).toBe("once");

    // A replacing host socket gets it too.
    const second = await authHost(s.host);
    expect((await nextHook(second.c, s.seal)).deliveryId).toBe(id);
  });

  it("ignores an ack for an unknown delivery and rejects a malformed one", async () => {
    const s = await withHooks(1);
    s.c.send({ type: "hook.ack", delivery_id: "0000000000000-0000000000000000" });
    await ping(s.c);
    s.c.send({ type: "hook.ack", delivery_id: 5 });
    const err = await s.c.json();
    expect(err.type).toBe("error");
    expect(err.message).toMatch(/^hook\.ack: /);
  });

  it("expires undelivered entries after 24 h via the alarm", async () => {
    const s = await offlineWithHooks(1);
    const fresh = await accepted(await postHook(s.rid, s.hookIds[0]!));
    const stub = storageOf(s.rid);
    const old = "0000000000001-0000000000000000";
    await runInDurableObject(stub, (_i, state) => {
      state.storage.kv.put(`hookq:${old}`, { sealed: "AAAA", at: 1 });
    });
    expect(await queuedIds(s.rid)).toEqual([old, fresh]);
    expect(await runDurableObjectAlarm(stub)).toBe(true);
    expect(await queuedIds(s.rid)).toEqual([fresh]);
    const alarm = await runInDurableObject(stub, (_i, state) => state.storage.getAlarm());
    expect(alarm).toBeGreaterThan(Date.now() + 23 * 3600_000);
  });
});

describe("hooks.set", () => {
  it("stores the seal key and hook hashes, and replacing the set removes old hooks", async () => {
    const s = await withHooks(2);
    const [a, b] = s.hookIds as [string, string];
    const stub = storageOf(s.rid);
    const keys = () =>
      runInDurableObject(stub, (_i, state) =>
        [...state.storage.kv.list({ prefix: "hook" })].map(([k]) => k),
      );
    expect((await keys()).sort()).toEqual(
      ["hook_key", `hook:${await hookHash(a)}`, `hook:${await hookHash(b)}`].sort(),
    );
    expect(await runInDurableObject(stub, (_i, state) => state.storage.kv.get("hook_key"))).toBe(
      s.seal.publicKey,
    );

    await accepted(await postHook(s.rid, a));
    await nextHook(s.c, s.seal);

    const c = newHookId();
    await setHooks(s.c, s.seal, [b, c]);
    expect((await postHook(s.rid, a)).status).toBe(404);
    await accepted(await postHook(s.rid, b));
    await nextHook(s.c, s.seal);
    await accepted(await postHook(s.rid, c));
    await nextHook(s.c, s.seal);
    expect((await keys()).filter((k) => k.startsWith("hook:")).sort()).toEqual(
      [`hook:${await hookHash(b)}`, `hook:${await hookHash(c)}`].sort(),
    );

    await setHooks(s.c, s.seal, []);
    expect((await postHook(s.rid, b)).status).toBe(404);
  });

  it("seals to the latest seal key", async () => {
    const s = await withHooks(1);
    const rotated = await newSealKey();
    await setHooks(s.c, rotated, s.hookIds);
    await accepted(await postHook(s.rid, s.hookIds[0]!));
    await nextHook(s.c, rotated);
  });

  it("answers a malformed hooks.set with error and keeps the old set", async () => {
    const s = await withHooks(1);
    const good = await hookHash(s.hookIds[0]!);
    const bad: Record<string, unknown>[] = [
      { type: "hooks.set", hooks: [good] },
      { type: "hooks.set", seal_key: "short", hooks: [good] },
      { type: "hooks.set", seal_key: b64urlEncode(new Uint8Array(31)), hooks: [good] },
      { type: "hooks.set", seal_key: s.seal.publicKey },
      { type: "hooks.set", seal_key: s.seal.publicKey, hooks: good },
      { type: "hooks.set", seal_key: s.seal.publicKey, hooks: [good, "nope"] },
      { type: "hooks.set", seal_key: s.seal.publicKey, hooks: [b64urlEncode(new Uint8Array(33))] },
      {
        type: "hooks.set",
        seal_key: s.seal.publicKey,
        hooks: Array.from({ length: MAX_HOOKS + 1 }, (_, i) => b64urlEncode(new Uint8Array(32).fill(i % 256))),
      },
    ];
    for (const m of bad) {
      s.c.send(m);
      const err = await s.c.json();
      expect(err.type).toBe("error");
      expect(err.message).toMatch(/^hooks\.set: /);
    }
    await accepted(await postHook(s.rid, s.hookIds[0]!));
    await nextHook(s.c, s.seal);

    // 256 distinct hooks are fine.
    const many = Array.from({ length: MAX_HOOKS }, newHookId);
    await setHooks(s.c, s.seal, many);
    await accepted(await postHook(s.rid, many[MAX_HOOKS - 1]!));
  });
});

describe("hook limits", () => {
  it("503s once the queue is full, and accepts again after an ack", async () => {
    const perHook = 50;
    const n = Math.ceil(MAX_QUEUED_HOOKS / perHook);
    const s = await offlineWithHooks(n);
    let sent = 0;
    for (const h of s.hookIds) {
      for (let i = 0; i < perHook && sent < MAX_QUEUED_HOOKS; i++, sent++) {
        await accepted(await postHook(s.rid, h));
      }
    }
    const full = await postHook(s.rid, s.hookIds[0]!);
    expect(full.status).toBe(503);
    expect(await full.json()).toEqual({ error: "queue full" });

    const { c } = await authHost(s.host);
    let first = "";
    for (let i = 0; i < MAX_QUEUED_HOOKS; i++) {
      const m = await c.json();
      expect(m.type).toBe("hook");
      if (i === 0) first = m.delivery_id as string;
    }
    c.send({ type: "hook.ack", delivery_id: first });
    await ping(c);
    await accepted(await postHook(s.rid, s.hookIds[1]!));
  });

  it("429s more than the per-hook rate limit per minute", async () => {
    const s = await withHooks(2);
    const [a, b] = s.hookIds as [string, string];
    for (let i = 0; i < HOOK_RATE_LIMIT; i++) {
      await accepted(await postHook(s.rid, a));
      s.c.send({ type: "hook.ack", delivery_id: (await nextHook(s.c, s.seal)).deliveryId });
    }
    const limited = await postHook(s.rid, a);
    expect(limited.status).toBe(429);
    await accepted(await postHook(s.rid, b)); // per hook
  });
});
