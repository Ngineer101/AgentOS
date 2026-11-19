import { env, exports } from "cloudflare:workers";
import { evictDurableObject, runDurableObjectAlarm, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { b64urlEncode } from "../src/codec";
import { MAX_FRAME } from "../src/relay";
import {
  BASE,
  connect,
  connectDevice,
  connectHost,
  connectPair,
  frame,
  newIdentity,
  open,
  ping,
  relayId,
  sign,
  tokenHash,
  unframe,
} from "./helpers";

const fetchPath = (path: string, init?: RequestInit) =>
  exports.default.fetch(`${BASE}${path}`, init);

describe("http", () => {
  it("serves /health", async () => {
    const res = await fetchPath("/health");
    expect(res.status).toBe(200);
    expect(await res.text()).toBe("ok");
  });

  it("rejects a bad relay_id with 400", async () => {
    for (const id of ["abc", "0123456789ABCDEF0123456789abcdef", "0".repeat(33), "g".repeat(32)]) {
      const res = await fetchPath(`/v1/relay/${id}/host`, { headers: { Upgrade: "websocket" } });
      expect(res.status).toBe(400);
    }
  });

  it("rejects non-websocket requests with 426", async () => {
    const res = await fetchPath(`/v1/relay/${"a".repeat(32)}/device`);
    expect(res.status).toBe(426);
  });

  it("404s unknown paths", async () => {
    expect((await fetchPath("/nope")).status).toBe(404);
    expect((await fetchPath(`/v1/relay/${"a".repeat(32)}/admin`)).status).toBe(404);
  });
});

describe("host auth", () => {
  it("accepts a good signature and sends a challenge of 32 bytes", async () => {
    const host = await newIdentity();
    const rid = await relayId(host);
    const { c, nonce } = await open(rid, "host");
    expect(nonce).toMatch(/^[A-Za-z0-9_-]{43}$/);
    c.send({
      type: "auth",
      role: "host",
      public_key: host.publicKey,
      signature: await sign(host, "host", rid, nonce),
    });
    const ready = await c.json();
    expect(ready.type).toBe("ready");
    expect(typeof ready.conn_id).toBe("string");
  });

  it("stores the host public key", async () => {
    const host = await newIdentity();
    const { rid } = await connectHost(host);
    const stub = env.RELAY.get(env.RELAY.idFromName(rid));
    const stored = await runInDurableObject(stub, (_i, state) => state.storage.kv.get("host_key"));
    expect(stored).toBe(host.publicKey);
  });

  it("rejects a bad signature with 4403", async () => {
    const host = await newIdentity();
    const other = await newIdentity();
    const rid = await relayId(host);
    const { c, nonce } = await open(rid, "host");
    c.send({
      type: "auth",
      role: "host",
      public_key: host.publicKey,
      signature: await sign(other, "host", rid, nonce),
    });
    expect(await c.closeCode()).toBe(4403);
  });

  it("rejects a signature over the wrong role", async () => {
    const host = await newIdentity();
    const rid = await relayId(host);
    const { c, nonce } = await open(rid, "host");
    c.send({
      type: "auth",
      role: "host",
      public_key: host.publicKey,
      signature: await sign(host, "device", rid, nonce),
    });
    expect(await c.closeCode()).toBe(4403);
  });

  it("rejects a key that does not hash to the relay_id with 4403", async () => {
    const host = await newIdentity();
    const intruder = await newIdentity();
    const rid = await relayId(host);
    const { c, nonce } = await open(rid, "host");
    c.send({
      type: "auth",
      role: "host",
      public_key: intruder.publicKey,
      signature: await sign(intruder, "host", rid, nonce),
    });
    expect(await c.closeCode()).toBe(4403);
  });

  it("closes with 4400 on anything before auth", async () => {
    const host = await newIdentity();
    const rid = await relayId(host);
    const a = await open(rid, "host");
    a.c.send({ type: "ping2" });
    expect(await a.c.closeCode()).toBe(4400);
    const b = await open(rid, "host");
    b.c.send(new Uint8Array([1, 2, 3]));
    expect(await b.c.closeCode()).toBe(4400);
    const d = await open(rid, "host");
    d.c.send({ type: "auth", role: "host", public_key: "!!", signature: "x" });
    expect(await d.c.closeCode()).toBe(4400);
  });

  it("replaces an older host socket with 4409", async () => {
    const host = await newIdentity();
    const first = await connectHost(host);
    const second = await connectHost(host);
    expect(await first.c.closeCode()).toBe(4409);
    await ping(second.c); // the new one is alive
  });

  it("answers ping with pong (auto-response and handler)", async () => {
    const host = await newIdentity();
    const { c } = await connectHost(host);
    c.send('{"type":"ping"}');
    expect(await c.json()).toEqual({ type: "pong" });
    await ping(c);
  });

  it("echoes a ping's id once the control messages before it are handled", async () => {
    const host = await newIdentity();
    const { c } = await connectHost(host);
    c.send({ type: "device.allow", public_key: "short" });
    c.send({ type: "ping", id: 7 });
    expect((await c.json()).type).toBe("error");
    expect(await c.json()).toEqual({ type: "pong", id: 7 });
  });

  it("answers bad control messages with error and stays open", async () => {
    const host = await newIdentity();
    const { c } = await connectHost(host);
    c.send({ type: "device.allow", public_key: "short" });
    expect((await c.json()).type).toBe("error");
    c.send("not json");
    expect((await c.json()).type).toBe("error");
    await ping(c);
  });
});

describe("device auth", () => {
  it("accepts an allowed device and announces it to the host", async () => {
    const host = await newIdentity();
    const dev = await newIdentity();
    const { c: h, rid } = await connectHost(host, [dev]);
    const d = await connectDevice(rid, dev);
    const ready = await d.json();
    expect(ready.type).toBe("ready");
    const opened = await h.json();
    expect(opened).toEqual({
      type: "open",
      conn_id: ready.conn_id,
      role: "device",
      public_key: dev.publicKey,
    });
  });

  it("accepts a device added with device.allow", async () => {
    const host = await newIdentity();
    const dev = await newIdentity();
    const { c: h, rid } = await connectHost(host);
    h.send({ type: "device.allow", public_key: dev.publicKey });
    await ping(h);
    const d = await connectDevice(rid, dev);
    expect((await d.json()).type).toBe("ready");
  });

  it("rejects an unknown device key with 4403", async () => {
    const host = await newIdentity();
    const dev = await newIdentity();
    const stranger = await newIdentity();
    const { rid } = await connectHost(host, [dev]);
    const d = await connectDevice(rid, stranger);
    expect(await d.closeCode()).toBe(4403);
  });

  it("rejects an allowed key with a bad signature with 4403", async () => {
    const host = await newIdentity();
    const dev = await newIdentity();
    const other = await newIdentity();
    const { rid } = await connectHost(host, [dev]);
    const { c, nonce } = await open(rid, "device");
    c.send({
      type: "auth",
      role: "device",
      public_key: dev.publicKey,
      signature: await sign(other, "device", rid, nonce),
    });
    expect(await c.closeCode()).toBe(4403);
  });

  it("closes with 4404 when the host is offline", async () => {
    const host = await newIdentity();
    const dev = await newIdentity();
    const rid = await relayId(host);
    const d = await connectDevice(rid, dev);
    expect(await d.closeCode()).toBe(4404);
  });

  it("revoke closes the device with 4401 and blocks reconnects", async () => {
    const host = await newIdentity();
    const dev = await newIdentity();
    const { c: h, rid } = await connectHost(host, [dev]);
    const d = await connectDevice(rid, dev);
    const { conn_id } = await d.json();
    await h.json(); // open
    h.send({ type: "device.revoke", public_key: dev.publicKey });
    expect(await d.closeCode()).toBe(4401);
    expect(await h.json()).toEqual({ type: "closed", conn_id });
    const again = await connectDevice(rid, dev);
    expect(await again.closeCode()).toBe(4403);
  });

  it("devices.set persists across host reconnects and drops removed keys", async () => {
    const host = await newIdentity();
    const a = await newIdentity();
    const b = await newIdentity();
    const first = await connectHost(host, [a, b]);
    first.c.ws.close(1000);
    await first.c.closed;
    // Reconnect without resending the set: storage still has it.
    const rid = first.rid;
    const { c: h, nonce } = await open(rid, "host");
    h.send({
      type: "auth",
      role: "host",
      public_key: host.publicKey,
      signature: await sign(host, "host", rid, nonce),
    });
    expect((await h.json()).type).toBe("ready");
    const da = await connectDevice(rid, a);
    expect((await da.json()).type).toBe("ready");
    await h.json(); // open
    h.send({ type: "devices.set", public_keys: [b.publicKey] });
    expect(await da.closeCode()).toBe(4401);
    const db = await connectDevice(rid, b);
    expect((await db.json()).type).toBe("ready");
  });
});

describe("forwarding", () => {
  it("forwards frames both ways with the conn_id prefix on the host side only", async () => {
    const host = await newIdentity();
    const dev = await newIdentity();
    const { c: h, rid } = await connectHost(host, [dev]);
    const d = await connectDevice(rid, dev);
    const { conn_id } = (await d.json()) as { conn_id: string };
    await h.json(); // open

    d.send(new Uint8Array([1, 2, 3, 4]));
    const up = unframe(await h.binary());
    expect(up.connId).toBe(conn_id);
    expect([...up.payload]).toEqual([1, 2, 3, 4]);

    h.send(frame(conn_id, new Uint8Array([9, 8, 7])));
    expect([...(await d.binary())]).toEqual([9, 8, 7]);

    // Unknown conn_id is dropped silently.
    h.send(frame("nope", new Uint8Array([5])));
    expect(await d.silent()).toBe(true);
    await ping(h);
  });

  it("keeps two devices separate", async () => {
    const host = await newIdentity();
    const a = await newIdentity();
    const b = await newIdentity();
    const { c: h, rid } = await connectHost(host, [a, b]);
    const da = await connectDevice(rid, a);
    const ia = (await da.json()).conn_id as string;
    await h.json();
    const db = await connectDevice(rid, b);
    const ib = (await db.json()).conn_id as string;
    await h.json();
    expect(ia).not.toBe(ib);
    h.send(frame(ib, new Uint8Array([2])));
    h.send(frame(ia, new Uint8Array([1])));
    expect([...(await da.binary())]).toEqual([1]);
    expect([...(await db.binary())]).toEqual([2]);
  });

  it("closes a device that sends a text frame after ready with 4400", async () => {
    const host = await newIdentity();
    const dev = await newIdentity();
    const { c: h, rid } = await connectHost(host, [dev]);
    const d = await connectDevice(rid, dev);
    const { conn_id } = await d.json();
    await h.json();
    d.send({ type: "hello" });
    expect(await d.closeCode()).toBe(4400);
    expect(await h.json()).toEqual({ type: "closed", conn_id });
  });

  it("closes an oversized frame with 4413", async () => {
    const host = await newIdentity();
    const dev = await newIdentity();
    const { c: h, rid } = await connectHost(host, [dev]);
    const d = await connectDevice(rid, dev);
    await d.json();
    await h.json();
    d.send(new Uint8Array(MAX_FRAME)); // exactly the limit is fine
    expect((await h.binary()).length).toBeGreaterThan(MAX_FRAME);
    d.send(new Uint8Array(MAX_FRAME + 1));
    expect(await d.closeCode()).toBe(4413);
  });

  it("host close message closes one device connection", async () => {
    const host = await newIdentity();
    const dev = await newIdentity();
    const { c: h, rid } = await connectHost(host, [dev]);
    const d = await connectDevice(rid, dev);
    const { conn_id } = await d.json();
    await h.json();
    h.send({ type: "close", conn_id, code: 4000 });
    expect(await d.closeCode()).toBe(4000);
  });

  it("device close is reported to the host", async () => {
    const host = await newIdentity();
    const dev = await newIdentity();
    const { c: h, rid } = await connectHost(host, [dev]);
    const d = await connectDevice(rid, dev);
    const { conn_id } = await d.json();
    await h.json();
    d.ws.close(1000);
    expect(await h.json()).toEqual({ type: "closed", conn_id });
  });

  it("host disconnect closes devices with 4404", async () => {
    const host = await newIdentity();
    const dev = await newIdentity();
    const { c: h, rid } = await connectHost(host, [dev]);
    const d = await connectDevice(rid, dev);
    await d.json();
    await h.json();
    h.ws.close(1000);
    expect(await d.closeCode()).toBe(4404);
  });

  it("host replacement closes the old host's devices with 4404", async () => {
    const host = await newIdentity();
    const dev = await newIdentity();
    const first = await connectHost(host, [dev]);
    const d = await connectDevice(first.rid, dev);
    await d.json();
    await first.c.json();
    const second = await connectHost(host, [dev]);
    expect(await first.c.closeCode()).toBe(4409);
    expect(await d.closeCode()).toBe(4404);
    const d2 = await connectDevice(second.rid, dev);
    expect((await d2.json()).type).toBe("ready");
  });
});

describe("pairing", () => {
  it("pairing slots are single use", async () => {
    const host = await newIdentity();
    const { c: h, rid } = await connectHost(host);
    const token = crypto.getRandomValues(new Uint8Array(32));
    h.send({ type: "pairing.open", token_hash: await tokenHash(token), expires_at: Date.now() + 60_000 });
    await ping(h);

    const p = await connectPair(rid, token);
    const ready = await p.json();
    expect(ready.type).toBe("ready");
    expect(await h.json()).toEqual({ type: "open", conn_id: ready.conn_id, role: "pair" });

    // Pair sockets forward like device sockets.
    p.send(new Uint8Array([42]));
    const f = unframe(await h.binary());
    expect(f.connId).toBe(ready.conn_id);
    expect([...f.payload]).toEqual([42]);

    const second = await connectPair(rid, token);
    expect(await second.closeCode()).toBe(4403);
  });

  it("rejects an unknown token with 4403", async () => {
    const host = await newIdentity();
    const { rid } = await connectHost(host);
    const p = await connectPair(rid, new Uint8Array([1, 2, 3]));
    expect(await p.closeCode()).toBe(4403);
  });

  it("rejects pairing with 4404 when the host is offline", async () => {
    const host = await newIdentity();
    const p = await connectPair(await relayId(host), new Uint8Array([1, 2, 3]));
    expect(await p.closeCode()).toBe(4404);
  });

  it("expired slots are rejected and cleaned up", async () => {
    const host = await newIdentity();
    const { c: h, rid } = await connectHost(host);
    const token = crypto.getRandomValues(new Uint8Array(32));
    const hash = await tokenHash(token);
    h.send({ type: "pairing.open", token_hash: hash, expires_at: Date.now() + 150 });
    await ping(h);
    await new Promise((r) => setTimeout(r, 300));
    const stub = env.RELAY.get(env.RELAY.idFromName(rid));
    await runDurableObjectAlarm(stub);
    const left = await runInDurableObject(stub, (_i, s) => [...s.storage.kv.list({ prefix: "pair:" })]);
    expect(left).toEqual([]);
    const p = await connectPair(rid, token);
    expect(await p.closeCode()).toBe(4403);
  });

  it("an expired slot is refused even before the alarm runs", async () => {
    const host = await newIdentity();
    const { c: h, rid } = await connectHost(host);
    const token = crypto.getRandomValues(new Uint8Array(32));
    h.send({ type: "pairing.open", token_hash: await tokenHash(token), expires_at: Date.now() + 100 });
    await ping(h);
    await new Promise((r) => setTimeout(r, 200));
    const p = await connectPair(rid, token);
    expect(await p.closeCode()).toBe(4403);
  });

  it("pairing.close closes a slot early", async () => {
    const host = await newIdentity();
    const { c: h, rid } = await connectHost(host);
    const token = crypto.getRandomValues(new Uint8Array(32));
    const hash = await tokenHash(token);
    h.send({ type: "pairing.open", token_hash: hash, expires_at: Date.now() + 60_000 });
    h.send({ type: "pairing.close", token_hash: hash });
    await ping(h);
    const p = await connectPair(rid, token);
    expect(await p.closeCode()).toBe(4403);
  });

  it("stores only hashed tokens and keys", async () => {
    const host = await newIdentity();
    const dev = await newIdentity();
    const { c: h, rid } = await connectHost(host, [dev]);
    const token = crypto.getRandomValues(new Uint8Array(32));
    h.send({ type: "pairing.open", token_hash: await tokenHash(token), expires_at: Date.now() + 60_000 });
    await ping(h);
    const stub = env.RELAY.get(env.RELAY.idFromName(rid));
    const keys = await runInDurableObject(stub, (_i, s) =>
      [...s.storage.kv.list()].map(([k]) => k).sort(),
    );
    expect(keys).toEqual(
      [`dev:${dev.publicKey}`, "host_key", `pair:${await tokenHash(token)}`].sort(),
    );
    expect(keys.some((k) => k.includes(b64urlEncode(token)))).toBe(false);
  });
});

describe("auth timeout", () => {
  it("closes a socket that does not authenticate in time with 4408", async () => {
    const host = await newIdentity();
    const rid = await relayId(host);
    const { c } = await open(rid, "host");
    const stub = env.RELAY.get(env.RELAY.idFromName(rid));
    // Fast-forward: move the deadline into the past, then fire the alarm.
    await runInDurableObject(stub, (_i, s) => {
      for (const ws of s.getWebSockets()) {
        const att = ws.deserializeAttachment() as { deadline: number };
        att.deadline = Date.now() - 1;
        ws.serializeAttachment(att);
      }
    });
    await runDurableObjectAlarm(stub);
    expect(await c.closeCode()).toBe(4408);
  });

  it("rejects a late auth message with 4408", async () => {
    const host = await newIdentity();
    const rid = await relayId(host);
    const { c, nonce } = await open(rid, "host");
    const stub = env.RELAY.get(env.RELAY.idFromName(rid));
    await runInDurableObject(stub, (_i, s) => {
      for (const ws of s.getWebSockets()) {
        const att = ws.deserializeAttachment() as { deadline: number };
        att.deadline = Date.now() - 1;
        ws.serializeAttachment(att);
      }
    });
    c.send({
      type: "auth",
      role: "host",
      public_key: host.publicKey,
      signature: await sign(host, "host", rid, nonce),
    });
    expect(await c.closeCode()).toBe(4408);
  });
});

describe("hibernation", () => {
  it("keeps forwarding and pairing slots across an eviction", async () => {
    const host = await newIdentity();
    const dev = await newIdentity();
    const { c: h, rid } = await connectHost(host, [dev]);
    const d = await connectDevice(rid, dev);
    const { conn_id } = (await d.json()) as { conn_id: string };
    await h.json();
    const token = crypto.getRandomValues(new Uint8Array(32));
    h.send({ type: "pairing.open", token_hash: await tokenHash(token), expires_at: Date.now() + 60_000 });
    await ping(h);

    const stub = env.RELAY.get(env.RELAY.idFromName(rid));
    await evictDurableObject(stub);

    d.send(new Uint8Array([7]));
    const up = unframe(await h.binary());
    expect(up.connId).toBe(conn_id);
    h.send(frame(conn_id, new Uint8Array([8])));
    expect([...(await d.binary())]).toEqual([8]);
    const p = await connectPair(rid, token);
    expect((await p.json()).type).toBe("ready");
  });

  it("delivers a frame a device pipelines right after its auth", async () => {
    const host = await newIdentity();
    const dev = await newIdentity();
    const { c: h, rid } = await connectHost(host, [dev]);
    const { c: d, nonce } = await open(rid, "device");
    d.send({
      type: "auth",
      role: "device",
      public_key: dev.publicKey,
      signature: await sign(dev, "device", rid, nonce),
    });
    d.send(new Uint8Array([1, 1]));
    const { conn_id } = await d.json();
    expect((await h.json()).type).toBe("open");
    const f = unframe(await h.binary());
    expect(f.connId).toBe(conn_id);
    expect([...f.payload]).toEqual([1, 1]);
  });
});

describe("connect helper", () => {
  it("connect() rejects a non-upgrade response", async () => {
    await expect(connect("/health")).rejects.toThrow();
  });
});
