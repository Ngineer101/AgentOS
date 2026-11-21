# Relay protocol (v1)

The relay lets a phone reach a host that listens on no network port. Both
dial out to the relay over WebSocket; the relay pairs sockets for one host and
forwards frames. **Everything the relay forwards between phones and the host
is Noise ciphertext.** It never sees prompts, replies, file names or
approvals, and it stores no messages.

**The one exception is webhook ingress** (see [Webhooks](#webhooks)): an
external service (Sentry, GitHub, …) POSTs a plain HTTPS request to the relay,
so the Worker necessarily sees that request in plaintext while it handles it.
It seals the request to the host's public key at once, in memory, and only the
sealed form is ever stored or forwarded. Whoever controls the Cloudflare
account the relay runs in (for a self-hosted relay: the user) could change the
Worker to keep the plaintext, so that account is trusted with webhook
contents. It is not trusted with anything else.

Implementation: one Cloudflare Durable Object per host (`relay/`). A
compatible in-process implementation for tests lives in
`crates/host/src/relay_dev.rs`.

## Identities

| Key | Algorithm | Used for |
| --- | --- | --- |
| Host relay key | Ed25519 | Proving to the relay that a socket is the host |
| Device relay key | Ed25519 | Proving to the relay that a socket is an allowed device |
| Host static key | X25519 | Noise handshakes (end-to-end) |
| Device static key | X25519 | Noise handshakes (end-to-end) |

**Relay id.** A host is addressed by its relay id: the first 32 lowercase hex
characters of `SHA-256(host relay public key)`. The relay checks that the key a
host presents hashes to the id in the URL, so nobody can claim another host's
id, and no first-come registration is needed.

All binary values in JSON are **base64url without padding**.

## Endpoints

```
GET /v1/relay/{relay_id}/host     WebSocket: the host (one at a time)
GET /v1/relay/{relay_id}/device   WebSocket: a paired device
GET /v1/relay/{relay_id}/pair     WebSocket: a device that is pairing
POST /v1/hook/{relay_id}/{hook_id} Webhook ingress (plain HTTPS, see Webhooks)
POST /v1/push/register            A phone's APNs token -> a handle (see Push notifications)
POST /v1/push/{relay_id}/send     The host sends a notification (see Push notifications)
GET /health                       200 "ok"
```

`relay_id` must match `^[0-9a-f]{32}$`, else HTTP 400. A non-WebSocket request
to a relay endpoint gets HTTP 426.

## Authentication (first messages, text frames)

1. Right after the upgrade the relay sends
   `{"type":"challenge","nonce":"<32 random bytes>"}`.
2. The client answers within 10 seconds with one of:

   - Host:
     `{"type":"auth","role":"host","public_key":"<ed25519 pub>","signature":"<sig>"}`
   - Device:
     `{"type":"auth","role":"device","public_key":"<ed25519 pub>","signature":"<sig>"}`
   - Pairing device:
     `{"type":"auth","role":"pair","pairing_token":"<token bytes>"}`

   The signature is Ed25519 over the UTF-8 bytes of
   `agenthost-relay-v1:{role}:{relay_id}:{nonce}` where `{nonce}` is the
   base64url string exactly as sent.

3. The relay checks:

   - host: `hex(sha256(public_key))[0..32] == relay_id` and the signature.
     If another host socket is open for this id, the **older one is closed**
     (code 4409) and the new one takes over. The relay stores the host public key.
   - device: the signature, and that `public_key` is in the allowed device set
     the host registered. Unknown or revoked key: close 4403.
   - pair: `sha256(pairing_token)` matches an open pairing slot that has not
     expired. The slot is **consumed** (single use) whether or not the pairing
     later succeeds. No match: close 4403.
   - device or pair with no host connected: close 4404 (`host offline`).

4. On success the relay sends `{"type":"ready","conn_id":"<id>"}` to the
   client. For device and pair sockets it also tells the host:
   `{"type":"open","conn_id":"<id>","role":"device"|"pair","public_key":"<ed25519 pub, device only>"}`.

Any failure closes the socket with a code from the table below. A client that
sends anything before `auth`, or a malformed `auth`, gets 4400.

## After authentication

### Device and pair sockets

Binary frames only. Each binary frame is one opaque payload (a Noise message)
forwarded to the host, prefixed as below. Text frames after `ready` close the
socket with 4400. Maximum frame size 65,600 bytes (4413 if exceeded).

### Host socket

**Binary frames** carry data for one device connection:

```
[1 byte: conn_id length N][N bytes: conn_id, ASCII][payload]
```

Relay → host frames use the same prefix to say which device sent them.
Host → relay frames are delivered to that device as a plain binary frame (the
prefix removed). A frame for an unknown conn_id is dropped.

**Text frames** are control messages:

Host → relay:

| Message | Effect |
| --- | --- |
| `{"type":"device.allow","public_key":"…"}` | Add a device relay key to the allowed set (persisted) |
| `{"type":"device.revoke","public_key":"…"}` | Remove it (persisted) and close its open sockets with 4401 at once |
| `{"type":"devices.set","public_keys":["…"]}` | Replace the allowed set (sent by the host after every connect) |
| `{"type":"pairing.open","token_hash":"<sha256 of token>","expires_at":<unix ms>}` | Open a single-use pairing slot (memory only) |
| `{"type":"pairing.close","token_hash":"…"}` | Close a slot early |
| `{"type":"close","conn_id":"…","code":4000}` | Close one device connection |
| `{"type":"hooks.set","seal_key":"<x25519 pub>","hooks":["<sha256 of hook_id>",…]}` | Replace the webhook seal key and the set of registered hooks (persisted; sent after every connect and whenever automations change). See [Webhooks](#webhooks) |
| `{"type":"hook.ack","delivery_id":"…"}` | The host has a webhook delivery; the relay deletes it from its queue. Unknown id: ignored |
| `{"type":"ping"}` | Relay answers `{"type":"pong"}` (keepalive, answered at once) |
| `{"type":"ping","id":7}` | Relay answers `{"type":"pong","id":7}` after every control message sent before it (the host waits for this before showing a pairing code) |

Relay → host:

| Message | Meaning |
| --- | --- |
| `{"type":"open",…}` | See above |
| `{"type":"closed","conn_id":"…"}` | A device socket closed |
| `{"type":"pong"}` | Answer to ping, with its `id` if it had one |
| `{"type":"error","message":"…"}` | A control message was rejected (socket stays open) |
| `{"type":"hook","delivery_id":"…","sealed":"<sealed envelope>"}` | A webhook delivery (see [Webhooks](#webhooks)); repeated until acked |

When the host socket closes, the relay closes every device and pair socket
for that host with 4404.

## Webhooks

Automations on the host can be triggered by webhooks from external services.
Since the host listens on no port, the relay accepts the HTTP request for it,
seals it to the host and queues it until the host has it.

### Registration

The host picks, per automation, a random `hook_id` (22-64 characters of
`[A-Za-z0-9_-]`) that it gives to the external service as part of the URL
`https://<relay>/v1/hook/{relay_id}/{hook_id}`, and an X25519 key pair (the
**seal key**) whose private half never leaves the host. It registers both with

```
{"type":"hooks.set","seal_key":"<32-byte X25519 public key>","hooks":["<base64url(SHA-256(UTF-8 hook_id))>", …]}
```

This replaces the whole set. The relay stores the seal key and one entry per
hash; it never learns the hook ids themselves except when a request arrives.
`seal_key` and every hash must be exactly 32 bytes, and there may be at most
256 hooks; otherwise the relay answers `{"type":"error","message":"hooks.set: …"}`
and keeps the previous set. Queued deliveries for a hook that is removed stay
until they are acked or expire.

The relay never holds webhook secrets: **signature verification (e.g.
`sentry-hook-signature`, `x-hub-signature-256`) happens on the host**, which
gets every header and the exact body bytes.

### Ingress

```
POST /v1/hook/{relay_id}/{hook_id}
```

The relay answers at once and never waits for the host (Sentry, for one,
requires an answer within 1 second). Bodies are small JSON objects; nothing
from the request is echoed back.

| Status | Body | When |
| --- | --- | --- |
| 202 | `{"delivery_id":"…"}` | Sealed and queued (written to storage before answering) |
| 400 | `{"error":"bad relay_id"}` | `relay_id` does not match `^[0-9a-f]{32}$` |
| 404 | `{"error":"unknown hook"}` | `hook_id` does not match `^[A-Za-z0-9_-]{22,64}$`, no seal key is registered, or the hook's hash is not registered |
| 405 | `{"error":"method not allowed"}` | Any method but POST (with `Allow: POST`) |
| 413 | `{"error":"payload too large"}` | Body over 262,144 bytes (by `Content-Length`, or by the bytes actually read) |
| 429 | `{"error":"rate limited"}` | More than 60 requests to this hook in the last minute (best effort, per relay instance; `Retry-After: 60`) |
| 503 | `{"error":"queue full"}` | 200 deliveries are already queued for this host (`Retry-After: 60`) |

Checks run in this order: relay_id, hook_id shape, method, registration,
`Content-Length`, rate limit, queue size, body size while reading. Every
request to a registered hook counts towards its rate limit, whatever its
outcome.

**delivery_id** is `<unix ms, zero-padded to 13 digits>-<16 lowercase hex random>`,
so sorting ids lexically sorts deliveries by arrival. (Within one relay
instance the millisecond part is strictly increasing; in a burst it can run a
few ms ahead of `received_at`.)

### Envelope

The plaintext the relay seals, UTF-8 JSON, which only ever exists in the
Worker's memory:

```
{"v":1,"hook_id":"<hook_id>","delivery_id":"<id>","received_at":<unix ms>,
 "headers":[["<lowercased name>","<value>"],…],"body":"<base64url of the exact raw body bytes>"}
```

`headers` has every request header except names starting with `cf-` and
`cookie`, `x-forwarded-for`, `x-real-ip`, `true-client-ip` (the runtime joins
repeated headers with `, `). The body is not parsed or re-encoded: it may be
any bytes, UTF-8 or not.

### Sealing

With WebCrypto, per delivery:

1. Generate an ephemeral X25519 key pair; `eph_pub` is its raw 32-byte public key.
2. `shared = X25519(eph_priv, seal_key)` (32 bytes).
3. `key = HKDF-SHA256(ikm = shared, salt = eph_pub || seal_key (64 bytes), info = "agenthost-hook-v1", L = 32)`.
4. `nonce` = 12 random bytes; `ct = AES-256-GCM(key, nonce, plaintext = envelope, aad = UTF-8 delivery_id)`, with the 16-byte tag appended.
5. `sealed = eph_pub (32) || nonce (12) || ct`, sent and stored as base64url.

The host opens it with its seal private key, checks the tag (which also binds
the `delivery_id` it was sent under), and verifies the service's signature
itself. Implementation: `relay/src/seal.ts` (`sealToHost`).

### Queue and delivery

Each accepted delivery is stored as `hookq:<delivery_id>` →
`{"sealed":"…","at":<received_at>}` **before** the 202 is sent. Then:

- if the host socket is ready, it gets `{"type":"hook","delivery_id":"…","sealed":"…"}` at once;
- whenever a host socket becomes ready, it gets every queued delivery, oldest
  first, right after its `ready`;
- the host answers `{"type":"hook.ack","delivery_id":"…"}` once it has the
  delivery (stored it, or decided to drop it), and the relay deletes it.

Delivery is at least once: an unacked delivery is sent again on every host
connect, so the host dedupes by `delivery_id`. Deliveries older than 24 hours
are deleted unacked (by an alarm, and skipped on connect).

`hook` frames are larger than the 65,600-byte frame limit (which applies only
to frames *from* clients and the host): a 262,144-byte body seals to about
350 KB of base64url, plus headers. The host must accept text frames of up to
1 MiB.

## Push notifications

Apple accepts notifications for an app only with an APNs key from the Apple
Developer team that signs it. So whoever publishes the iOS app runs a relay
with that team's key (Worker secrets `APNS_KEY`, the `.p8` PEM, and
`APNS_KEY_ID`, `APNS_TEAM_ID`, `APNS_TOPIC`, the app's bundle id). Hosts
then need no key, whichever relay they connect through: a build names its
relay in Info.plist (`AgentHostPushRelay`), and without one the phone uses
the relay it is paired through. A relay without the secrets answers 404 on
both routes, and the phone falls back to giving the host its raw token (the
host then needs its own key, `push.set_key`).

### Registration (the phone)

```
POST /v1/push/register
{"token": "<hex>", "topic": "<bundle id>", "environment": "sandbox"|"production", "relay_id": "<host>"}
-> 200 {"handle": "<base64url>"}
   400 malformed, 403 a topic this relay doesn't send for, 404 no APNs key here
```

The handle is `iv(12) ‖ AES-256-GCM(JSON {v:1, t: token, e: environment,
r: relay_id})` with additional data `agenthost-push-handle-v1:<topic>`,
under a key derived from `APNS_KEY` (HKDF-SHA256, empty salt, info
`agenthost-push-handle-v1`). Only this relay can open it, and it names the
one host that may use it; the phone gives the host the handle, never the
token. Registration is open (only someone who has a token can make a handle
for it); nothing is stored.

### Sending (the host)

```
POST /v1/push/{relay_id}/send
x-agenthost-key: <host relay public key, base64url>
x-agenthost-time: <unix ms>
x-agenthost-signature: <Ed25519 over "agenthost-push-v1:send:{relay_id}:{time}:{base64url(sha256(body))}">
{"handle": "...", "sealed": "<base64url, at most 3000 chars>", "collapse_id": "<at most 64 bytes>"?}
```

The relay checks that the key's relay id is the one in the path, that the
time is within 5 minutes of its clock, the signature, and that the handle
opens and names this relay id; then at most 60 notifications per host per
minute (in that host's Durable Object, in memory). It sends Apple
`{"aps": {"alert": {"title": "AgentHost", "body": "A session needs you"},
"sound": "default", "mutable-content": 1}, "v": 1, "e": sealed}` with
`apns-push-type: alert`, priority 10, a 1-hour expiry and the collapse id,
signed with an ES256 provider token (`iat` rounded down to 40 minutes, so
every instance uses the same one). `sealed` is the content the host
encrypted to the phone (ChaCha20-Poly1305); the relay can't read it.
`BadDeviceToken` is retried in the other environment.

Answers, as APNs would: `200 {"environment"}` (the one that worked), or a
status with `{"reason"}`: Apple's own (`410 Unregistered` means the host
drops the phone; Apple's 5xx become 502), or the relay's `401 BadSignature`,
`400 BadHandle`/`BadPayload`/`BadCollapseId`, `413 PayloadTooLarge`, `429
TooManyRequests`, `404 PushUnavailable`, `502 ApnsUnreachable`.

## What the relay stores

Durable Object storage holds exactly:

- the host relay public key;
- the allowed device relay public keys;
- open pairing slots: the SHA-256 of each token and its expiry (deleted when
  used, closed or expired);
- the host's webhook seal public key, and the SHA-256 of each registered hook id;
- queued webhook deliveries, **sealed** to the host (deleted on ack or after 24 h).

Nothing else is written (push notifications store nothing: handles carry
the token, sealed, and the content is sealed to the phone). Device frames are never buffered: a device that is
offline simply misses nothing, because the host replays events from its own
log. Webhook plaintext (headers and body) exists only in memory while one
request is handled. Logs record only lifecycle events, for webhooks
`relay=<id> role=hook accepted|rejected <status>`, never headers, bodies, hook
ids or keys.

## Close codes

| Code | Meaning |
| --- | --- |
| 4400 | Protocol error |
| 4401 | Device revoked |
| 4403 | Not allowed (bad signature, unknown device, bad pairing token) |
| 4404 | Host offline |
| 4408 | Authentication timeout |
| 4409 | Replaced by a newer host connection |
| 4413 | Frame too large |

## End-to-end layer (inside the binary payloads)

Not visible to the relay; documented here for completeness. Implemented once in
`agenthost-proto::tunnel`.

- Pairing: `Noise_XXpsk3_25519_ChaChaPoly_BLAKE2s`, phone is the initiator,
  PSK = the 32-byte pairing secret from the QR code. The phone checks the host
  static key against the QR code. The phone's third handshake message carries
  `{"name": …, "relay_key": …}`. Both sides derive a 6-digit short code from
  the handshake hash; the user confirms it matches on the desktop.
- Every later connection: `Noise_IK_25519_ChaChaPoly_BLAKE2s`, phone is the
  initiator. The host accepts only device static keys it has stored.
- Transport: stateful Noise transport (implicit nonces). A replayed, dropped,
  reordered or altered frame fails decryption and the connection is closed.
- Each transport frame carries a 1-byte flag (`0` = more, `1` = final) then a
  chunk of one JSON-RPC message, so messages larger than a Noise frame work.
- Screen streams use a tunnel of their own (a second device socket, its own
  `Noise_IK` handshake). Its first message is the JSON-RPC request
  `screen.attach` with a one-time ticket from `screen.start` on the normal
  connection; after the response, every message on that tunnel is one
  [screen-relay](https://github.com/vercel-labs/screen-relay) `nsl1` message
  (H.264 frames from the host; ACKs and, when allowed, input from the phone),
  chunked the same way. The relay sees no difference.
