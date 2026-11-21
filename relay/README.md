# agenthost relay

A Cloudflare Worker plus one Durable Object per host that pairs WebSockets
from an agenthost host daemon and its phones, and forwards opaque binary frames
between them. Everything forwarded between phones and the host is end-to-end
Noise ciphertext; the relay cannot read it. The one exception is webhook
ingress: services like Sentry or GitHub POST plain HTTPS requests, which the
Worker seals to the host's X25519 key in memory as soon as they arrive, then
queues and forwards only in sealed form. So the owner of the Cloudflare account
it is deployed to sees (could log) webhook contents, nothing else. The wire protocol is specified in
[`../docs/relay-protocol.md`](../docs/relay-protocol.md); this is its
implementation.

- `GET /health`: `200 ok`
- `GET /v1/relay/{relay_id}/host`: the host (one at a time)
- `GET /v1/relay/{relay_id}/device`: a paired device
- `GET /v1/relay/{relay_id}/pair`: a device that is pairing
- `POST /v1/hook/{relay_id}/{hook_id}`: webhook ingress; answers `202
  {"delivery_id"}` at once (or 404/405/413/429/503) and never waits for the host

Each `relay_id` maps to its own Durable Object (`idFromName(relay_id)`). The
object uses the WebSocket Hibernation API, so idle connections are not billed
as duration. Per-socket state (role, conn_id, auth stage, challenge nonce,
device key) lives in socket attachments and survives hibernation.

## What it stores

Durable Object storage (SQLite-backed, key/value API) holds only:

| Key | Value |
| --- | --- |
| `host_key` | the host relay Ed25519 public key (base64url) |
| `dev:<public key>` | one entry per allowed device relay public key |
| `pair:<sha256(token)>` | an open pairing slot's `expires_at` (see below) |
| `hook_key` | the host's X25519 webhook seal public key (base64url) |
| `hook:<sha256(hook_id)>` | one entry per registered webhook |
| `hookq:<delivery_id>` | a queued webhook delivery, sealed to the host, and its arrival time; deleted on `hook.ack` or after 24 h |

There are no accounts. No device frame is buffered or stored; webhook
plaintext is never stored. Logs contain only lifecycle events
(`relay=<id> role=<role> connect|ready|close <code>`, and
`relay=<id> role=hook accepted|rejected <status>`), never payloads, headers,
hook ids, keys or tokens. Workers observability is turned off in
`wrangler.toml`.

## Deploy to your own Cloudflare account

```sh
cd relay
npm ci
npx wrangler login        # once, in a browser
npx wrangler deploy
```

Wrangler prints the URL, for example `https://agenthost-relay.<subdomain>.workers.dev`.
The host's relay URL is the WebSocket form of it:

```
wss://agenthost-relay.<subdomain>.workers.dev
```

(`wss://<name>.<subdomain>.workers.dev`, where `<name>` is `name` in
`wrangler.toml`). The host and phones append `/v1/relay/{relay_id}/...`.
Durable Objects with SQLite storage are available on the Workers Free plan.

## Run locally

```sh
npm ci
npx wrangler dev          # ws://localhost:8787
curl http://localhost:8787/health
```

## Tests

```sh
npm test                  # vitest in workerd via @cloudflare/vitest-plugin
npx tsc --noEmit
```

Tests generate Ed25519 and X25519 keys with WebCrypto, drive real WebSockets
through the Worker into the Durable Object, and open sealed webhook deliveries
the way the host does (`openFromRelay` in `test/helpers.ts`).

## Interpretation choices

Where the protocol document leaves room, the relay does this:

1. **Order of checks.** Path then `relay_id` (`400`) then upgrade (`426`).
   For device and pair sockets the relay checks that a host is connected
   *before* checking credentials, so a pairing token is never consumed while
   the host is offline (4404 wins over 4403).
2. **Malformed vs. not allowed.** Missing fields, wrong `role` for the
   endpoint, non-canonical base64url, a public key that is not 32 bytes or a
   signature that is not 64 bytes: 4400. Well-formed but failing signature,
   relay_id mismatch, unknown device key, unknown or expired token: 4403.
3. **Pairing slots are persisted** (as `pair:<token hash>` -> `expires_at`) so
   a pairing survives hibernation. They hold only the SHA-256 of the token,
   are deleted when used (whatever happens next), on `pairing.close`, and on
   expiry by an alarm; an expired slot is refused even if the alarm has not
   run yet. At most 64 slots may be open at once (`error` beyond that).
   `pairing.open` with `expires_at` in the past is rejected with `error`.
   Slots are not cleared when the host disconnects; they just expire.
   `token_hash` is base64url of the 32-byte digest; the token in `auth` is
   base64url and is hashed as raw bytes.
4. **Host replacement.** The older host gets 4409. Its device and pair sockets
   are closed with 4404, since their conn_ids and Noise sessions belong to the
   old host session; they reconnect to the new host.
5. **`devices.set` also revokes.** Any open device socket whose key is not in
   the new set is closed with 4401, same as `device.revoke`.
6. **`closed` notifications.** The host gets `{"type":"closed"}` whenever a
   ready device or pair socket goes away, including closes the host asked for
   and revocations. Sockets that never reached `ready` are not announced.
7. **`close` control message.** `code` defaults to 4000 and must be an integer
   in 4000-4999 (`error` otherwise). Unknown conn_id: ignored.
8. **Frame sizes.** 65,600 bytes is the limit on any frame from a device or
   pair socket, on any text frame, and on the payload part (after the prefix)
   of a host binary frame, whose total may be up to 65,856 bytes. Exceeding it
   closes that socket with 4413. The host must accept relay-to-host frames of
   up to 65,600 + 1 + conn_id length bytes.
9. **Malformed host binary frames** (empty, zero-length conn_id, truncated
   prefix) are dropped like frames for an unknown conn_id.
10. **conn_id** is 16 lowercase hex characters, unique among open sockets of
    that host. The host's own `ready` also carries a conn_id (unused).
11. **Ping.** The exact text `{"type":"ping"}` is answered with
    `{"type":"pong"}` by the runtime's hibernation auto-response, without
    waking the object, on any socket (even before auth). Other spellings of a
    ping from the host go through the normal handler and get the same answer.
12. **Pipelined frames.** Frames a client sends after `auth` but before it
    receives `ready` are held while the signature is verified and then
    processed in order (only while the object stays awake, which it does
    during verification).
13. **Auth timeout** is enforced by a Durable Object alarm 10 s after the
    upgrade (4408), and also checked when a late `auth` arrives.
14. Unknown host control message types get `{"type":"error"}`; the socket
    stays open.
15. **Webhook check order.** relay_id (400), hook_id shape (404), method
    (405), registration (404), `Content-Length` (413), rate limit (429), queue
    size (503), then the body is read with a cap (413). Every request to a
    registered hook counts towards its 60/minute limit, whatever its outcome.
    The rate limit is a sliding window kept in memory and resets when the
    object is evicted. A rejected request's body is still read (up to the
    limit) before answering, because workerd raises an error for an unread
    request stream.
16. **delivery_id ordering.** The millisecond part is
    `max(now, previous + 1)` within one object instance, so two requests in the
    same millisecond still sort in arrival order; `received_at` in the envelope
    is the real arrival time.
17. **Seal key changes.** If `hooks.set` changes the seal key while a request
    is being sealed, it is sealed again with the new key. Already-queued
    deliveries are not re-sealed.
18. **Expiry.** The alarm deletes queued deliveries older than 24 h; a host
    that connects is not sent ones that are older but not yet deleted.
