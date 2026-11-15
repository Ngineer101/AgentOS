# AgentHost for iOS

> **Status: never compiled; untested.** This app was written on a Linux
> machine without Xcode. Not one line of the Swift here has been through the
> Swift compiler, run in the Simulator or on a phone. Expect compile errors
> and behaviour bugs on first build. The Rust layer it sits on
> (`crates/ffi`) *is* built and tested (see "What is verified").

A native SwiftUI app (iOS 17+) for following and steering an agenthost from
your phone: pair with one host by QR code, read sessions, send prompts,
start a session in an existing workspace, and answer MCP approvals.

All security-relevant protocol code (relay authentication, the Noise
`XXpsk3` pairing and `IK` handshakes, framing, replay protection, JSON-RPC)
is Rust, shared with the host and CLI, reached through UniFFI bindings
(`Generated/agenthost_ffi.swift`). The Swift code only stores keys, renders
state and calls `RemoteClient.call(method:paramsJson:)`.

## Layout

| Path | What |
| --- | --- |
| `project.yml` | XcodeGen project (the `.xcodeproj` is generated, not committed) |
| `build-rust.sh` | Builds `crates/ffi` for `aarch64-apple-ios` and `aarch64-apple-ios-sim`, makes `Frameworks/AgentHostFFI.xcframework`, regenerates the bindings |
| `Generated/` | UniFFI output: `agenthost_ffi.swift` (compiled into the app), `agenthost_ffiFFI.h` and `module.modulemap` (packed into the XCFramework). Committed; regenerate after changing `crates/ffi` |
| `AgentHost/App` | App entry, lock / privacy cover, `AppModel` (connection, sync, cache, outbox) |
| `AgentHost/Core` | Protocol models, Keychain + Secure Enclave store, transcript model and cache |
| `AgentHost/Views` | The six screens: Pair, Sessions, Session, New session, Approval, Host |

## Building on a Mac

Prerequisites: Xcode 15 or newer (iOS 17 SDK), [rustup](https://rustup.rs),
[XcodeGen](https://github.com/yonaskolb/XcodeGen) (`brew install xcodegen`).
Apple Silicon Mac (the simulator slice is `aarch64-apple-ios-sim` only).

```sh
cd ios
./build-rust.sh              # release; PROFILE=debug ./build-rust.sh for a debug lib
xcodegen generate
open AgentHost.xcodeproj
```

Then pick your team under Signing & Capabilities (or set `DEVELOPMENT_TEAM`
in `project.yml`) and run on a device or an Apple Silicon simulator. The
Secure Enclave and Face ID paths need a real iPhone; in the Simulator the
credentials are stored without the Secure Enclave layer (see below) and
Face ID can be simulated from Features → Face ID.

To try it end to end without Cloudflare, run a dev relay and the host on
your Mac with the Mac's LAN address (the phone dials out to the relay):

```sh
agenthostd dev-relay --listen 0.0.0.0:8787 &
agenthostd run --relay-url ws://<mac-ip>:8787 --enable-fake-agent &
agenthost device pair          # shows the QR code; confirm the code here
```

The relay URL inside the QR code is the one the host was started with, so it
must be reachable from the phone.

## Behaviour

| Spec outcome | How |
| --- | --- |
| Face ID / passcode before any content, every launch and on return from background | `RootView` shows only `LockView` while `phase == .locked`; the app locks on `scenePhase == .background` (drops the connection and the in-memory keys) and unlocks with `LAContext.evaluatePolicy(.deviceOwnerAuthentication)` |
| Device keys in the Keychain, this device only, released after Face ID | `CredentialStore`: one generic-password item with `SecAccessControl(kSecAttrAccessibleWhenUnlockedThisDeviceOnly, .userPresence)`. On hardware with a Secure Enclave its contents are also ECIES-encrypted (P-256, AES-GCM) to a Secure Enclave key created with `.privateKeyUsage + .userPresence`; the key never leaves the Enclave. The already-evaluated `LAContext` from the lock screen is passed to both reads so there is one prompt. Simulator fallback: no Enclave layer (format byte 0) |
| App-switcher snapshot blanked | `PrivacyCover` over everything whenever `scenePhase != .active` |
| Last transcript readable offline, prompts disabled with a clear "offline" state | `TranscriptCache` writes JSON to Application Support with `.completeFileProtection` (and excludes it from backup); loaded after unlock. `ConnectionBanner` and the disabled prompt box say "Offline" / "Host offline" |
| No gaps, no duplicates after background or reconnect | `Transcript` keeps events contiguous by `seq`, drops duplicates, holds out-of-order events and re-subscribes from the last contiguous seq on a gap. On every connection the app calls `events.subscribe` with `after_seq = lastSeq` for opened and live sessions |
| Host offline: says so, reconnects by itself | Close code 4404 → "Host offline"; reconnect with exponential backoff (1 s … 30 s, jittered) and immediately when `NWPathMonitor` sees the network return |
| Revoked: "This device was removed", no readable content | `device.revoked` notification, close code 4401, or a 4403 refusal on connect → delete the Keychain item, the Enclave key and the cache, clear memory, show `RemovedView` with "Pair again" |
| Safe prompt retry | Each prompt gets a client-generated `prompt_id` (UUID) kept in an outbox until the host answers; resent with the same id after a reconnect (the host records it once) |
| Approval reaches the host within 2 s | `approval.respond` straight from `ApprovalView`; banners on the Sessions and Session screens while anything is `waiting_approval` |
| Capability errors and "update required" | Typed `AgentHostError.Rpc(code:…)` from Rust; `-32001` and `-32002` get specific messages. `hello` rejection stops reconnecting and shows "Update required" |

Unpair (Host screen) deletes this phone's keys and cache. A phone may not
call `device.revoke`, so the desktop should also revoke the device.

Not in v1 (per the spec): push notifications, adding workspaces or
connectors, policy changes, pairing other devices, multiple hosts, iPad
layout.

## What is verified

- `cargo test -p agenthost-ffi`: pairing through the FFI API against a real
  host and the dev relay (desktop confirms over the local socket, codes match),
  `RemoteClient.connect`, `session.list`, a fake-agent session with streamed
  `session.delta` and `event` notifications, idempotent prompt retry,
  capability and not-found errors, resume after disconnect with no gaps or
  duplicates, revocation (`on_closed` with 4401 in < 2.5 s, reconnect refused
  with 4403), refused and reused pairing codes, and host offline (4404).
- The C ABI through UniFFI-generated *Python* bindings against a real
  `agenthostd` + dev relay: async functions, the async constructor, foreign
  callback interfaces, typed errors and the closed callback (a manual smoke
  test, not committed).
- **Not verified:** anything in Swift, the XcodeGen project, `build-rust.sh`
  on macOS (the iOS cross-compile, the XCFramework), Keychain and Secure
  Enclave behaviour, the camera scanner, layout.
