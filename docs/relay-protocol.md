# Relay protocol (v1)

The relay lets a phone reach a host that listens on no network port. Both
dial out to the relay over WebSocket; the relay pairs sockets for one host and
forwards frames. **Everything the relay forwards is Noise ciphertext.** It
never sees prompts, replies, file names or approvals, and it stores no
messages.

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
| `{"type":"ping"}` | Relay answers `{"type":"pong"}` (keepalive, answered at once) |
| `{"type":"ping","id":7}` | Relay answers `{"type":"pong","id":7}` after every control message sent before it (the host waits for this before showing a pairing code) |

Relay → host:

| Message | Meaning |
| --- | --- |
| `{"type":"open",…}` | See above |
| `{"type":"closed","conn_id":"…"}` | A device socket closed |
| `{"type":"pong"}` | Answer to ping, with its `id` if it had one |
| `{"type":"error","message":"…"}` | A control message was rejected (socket stays open) |

When the host socket closes, the relay closes every device and pair socket
for that host with 4404.

## What the relay stores

Durable Object storage holds exactly: the host relay public key, and the
allowed device relay public keys. Pairing slots live in memory and expire.
Nothing else is written. Frames are never buffered: a device that is offline
simply misses nothing, because the host replays events from its own log.

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
