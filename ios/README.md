# AgentHost for iOS

> **Status: never compiled; untested.** This app was written on a Linux
> machine without Xcode. Not one line of the Swift here has been through the
> Swift compiler, run in the Simulator or on a phone. Expect compile errors
> and behaviour bugs on first build. The Rust layer it sits on
> (`crates/ffi`) *is* built and tested (see "What is verified").

A native SwiftUI app (iOS 17+) for following and steering an agenthost from
your phone: pair with one host by QR code, read sessions, send prompts
(with photos and files attached, or dictated), start a session in an
existing workspace, answer MCP approvals, and (optionally) get a push
notification when a session needs you.

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
| `Shared` | `PushCrypto.swift`, compiled into the app and the extension: the notification key in the shared Keychain group and the code that opens a notification's content |
| `AgentHostNotifications` | Notification Service Extension (`dev.agenthost.ios.notifications`): decrypts each push before it is shown |
| `AgentHost/Views` | The screens: Pair, Sessions, Session, New session (with the project folder picker), Approval, Settings, Notifications, Automations (list, detail with runs, editor; hosts on protocol 5). Dark only (`UIUserInterfaceStyle: Dark` plus `.preferredColorScheme(.dark)`), standard SwiftUI controls and system colours throughout, with iOS 26 Liquid Glass controls (`Glass.swift`: `.glassEffect`, `.glassProminent`, the bottom search toolbar) and a material look-alike on iOS 17–25; designs in the project's nano.design file |

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

For screenshots, Debug builds take `-AgentHostDemo` (sample host, sessions,
transcripts and an approval; no pairing, lock or network) and optionally
`-AgentHostDemoScreen session|dictating|new|new-login|picker|settings|archived|archived-session|pair|notifications|notifications-own-key|pair-notifications|pair-apns-key|automations|automation|automation-webhook|automation-new`
(`App/DemoData.swift`; compiled out of Release builds):

```sh
xcrun simctl launch booted dev.agenthost.ios -AgentHostDemo -AgentHostDemoScreen session
```

### Push notifications (optional)

Notifications need the Push Notifications capability, which needs a **paid
Apple Developer account** (a free Personal Team can't sign it; without it,
leave the feature off and everything else works).

1. Set `DEVELOPMENT_TEAM` for both targets, `AgentHost` and
   `AgentHostNotifications` (the Notification Service Extension,
   `dev.agenthost.ios.notifications`, embedded in the app). `xcodegen
   generate` writes both entitlements files from `project.yml`: the app has
   `aps-environment` (`development`; Xcode's distribution export switches it
   to `production`) and both have Keychain Sharing with the group
   `$(AppIdentifierPrefix)dev.agenthost.shared`, where the app keeps the
   notification key the extension reads. Automatic signing registers the
   App IDs and capabilities.
2. Create an APNs key: developer.apple.com → Certificates, Identifiers &
   Profiles → Keys → +, enable "Apple Push Notifications service (APNs)",
   download `AuthKey_<KEYID>.p8` (it can be downloaded only once).
3. Give it to your relay once ([relay/README.md](../relay/README.md#push-notifications-if-you-publish-the-ios-app)).
   Phones paired through it get notifications with nothing to set up on the
   host: the app registers its APNs token with the relay
   (`POST /v1/push/register`) and gives the host only the handle it gets
   back. For builds you hand out, set `AGENTHOST_PUSH_RELAY` to
   `https://<your relay>` so phones paired through other relays use yours.
4. Without such a relay (or with a host older than protocol 6), the host
   sends to Apple itself: after pairing the app asks for the `.p8` file
   (Settings → Notifications → "Add APNs Key…" later; it says plainly that
   nothing is sent until there is one). The key goes to the host
   (`push.set_key`), never stays on the phone; the team id is the app's own
   (`AppIdentifierPrefix` in Info.plist).

The app tells the host which APNs server its token belongs to: Debug
builds use the sandbox; other builds read `aps-environment` from the
embedded provisioning profile (`development` → sandbox, otherwise and for
App Store builds production).

## Behaviour

| Spec outcome | How |
| --- | --- |
| Face ID / passcode: optional, **off by default** | Settings → Security → "Require Face ID". On: `RootView` shows only `LockView` while `phase == .locked`, the app locks on `scenePhase == .background` (drops the connection and the in-memory keys) and unlocks with `LAContext.evaluatePolicy(.deviceOwnerAuthentication)`. Off: the keys load without a prompt and going to the background only drops the connection. Switching either way asks for Face ID or the passcode first and saves the keys again with the new protection; keys saved by an earlier version (always protected) need one last prompt, then are saved unprotected |
| Device keys in the Keychain, this device only | `CredentialStore`: one generic-password item, `kSecAttrAccessibleWhenUnlockedThisDeviceOnly` (never backed up or migrated), plus `.userPresence` when Face ID is required. On hardware with a Secure Enclave its contents are also ECIES-encrypted (P-256, AES-GCM) to a Secure Enclave key created with `.privateKeyUsage` (plus `.userPresence` when required); the key never leaves the Enclave. With Face ID on, the lock screen's evaluated `LAContext` is passed to both reads so there is one prompt. Simulator fallback: no Enclave layer (format byte 0) |
| App-switcher snapshot blanked | `PrivacyCover` over everything whenever `scenePhase != .active` |
| Last transcript readable offline, prompts disabled with a clear "offline" state | `TranscriptCache` writes JSON to Application Support with `.completeFileProtection` (and excludes it from backup); loaded after unlock. `ConnectionBanner` and the disabled prompt box say "Offline" / "Host offline" |
| No gaps, no duplicates after background or reconnect | `Transcript` keeps events contiguous by `seq`, drops duplicates, holds out-of-order events and re-subscribes from the last contiguous seq on a gap. On every connection the app calls `events.subscribe` with `after_seq = lastSeq` for opened and live sessions |
| Host offline: says so, reconnects by itself | Close code 4404 → "Host offline"; reconnect with exponential backoff (1 s … 30 s, jittered) and immediately when `NWPathMonitor` sees the network return |
| Revoked: "This device was removed", no readable content | `device.revoked` notification, close code 4401, or a 4403 refusal on connect → delete the Keychain item, the Enclave key and the cache, clear memory, show `RemovedView` with "Pair again" |
| Safe prompt retry | Each prompt gets a client-generated `prompt_id` (UUID) kept in an outbox until the host answers; resent with the same id after a reconnect (the host records it once) |
| Approval reaches the host within 2 s | `approval.respond` straight from `ApprovalView`; a status row on the Sessions list and a status card on the Session screen while anything is `waiting_approval` |
| Capability errors and "update required" | Typed `AgentHostError.Rpc(code:…)` from Rust; `-32001` and `-32002` get specific messages. `hello` rejection stops reconnecting and shows "Update required" |
| Attachments (protocol 2 hosts) | Composer paperclip menu: Photo Library (`PhotosPicker`; HEIC re-encoded as JPEG) or Files (`.fileImporter`, any type, several at once). Each pick is copied to a temporary file, sent with `RemoteClient.uploadFile` straight away (up to 25 MB, 10 per prompt), then deleted; the ids go in `session.prompt` `attachments`. Chips show progress and failures; Send waits for uploads. Unsent attachments stay with the session's draft when you leave it (uploads carry on; in memory, cleared on unpair). Sent attachments are listed under the prompt bubble; images show as pictures there (protocol 7 hosts keep them, or this phone's cache has them), and a tap opens them full screen (pinch or double-tap to zoom, Share) |
| Push notifications (protocol 4 hosts), optional, off until turned on | Offered once after pairing, when the phone has connected for the first time ("Turn On Notifications?", then "Add Your APNs Key" if the host has none, then a test notification; every step can be skipped), and in Settings → Notifications: on/off, what to be told about (approvals, on by default; finished turns; failed sessions → `push.set_prefs`), the host's APNs key, Send Test Notification, the last APNs error. On: `UNUserNotificationCenter.requestAuthorization`, `registerForRemoteNotifications`, then `push.register` with the token (hex), the bundle id, the APNs environment, the prefs and this phone's 32-byte notification key; repeated on every connection while on (tokens change). Off: `push.unregister` (or on the next connection when offline). The host sends only "A session needs you" in the clear; the real title and text are ChaCha20-Poly1305-sealed to the notification key (`e`, AAD `agenthost-push-v1`) and opened by the Notification Service Extension, which shows the generic alert if anything fails. The key is a Keychain item in the group shared with the extension, `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` (notifications arrive while locked) and not behind Face ID; it only opens notification content. Tapping a notification opens its session (an approval's session leads to the approval); in the foreground the notification is quiet when that session is on screen; opening a session clears its notifications. Turned off in iOS Settings: Settings → Notifications says so, with Open Settings. Unpair: `push.unregister` (best effort), then the key, the token registration and delivered notifications are deleted; the same when the device is removed |
| Voice input | Mic button uses iOS dictation (the Speech framework, `SFSpeechRecognizer` fed from `AVAudioEngine`, at most 5 minutes): the text appears while you talk and is added to the draft on Stop. iOS starts a new transcription after a pause (its results then hold only the words since), so the words before each restart are kept and the new ones added after them. Each session keeps its unsent draft while you look at another (in memory, cleared on unpair). Recognition is forced on device where the iPhone supports it, otherwise Apple's dictation service is used. Works with any host; the host's speech model (Nemotron) is only for the desktop, so nothing is downloaded for the phone. Needs speech recognition and microphone permission (`NSSpeechRecognitionUsageDescription`, `NSMicrophoneUsageDescription`); the photo picker needs none |

Unpair (Settings) deletes this phone's keys and cache. A phone may not
call `device.revoke`, so the desktop should also revoke the device.

Not in v1 (per the spec): adding workspaces or connectors, policy changes, pairing other devices, multiple hosts, iPad
layout.

## Sessions: status, archive, default logins

- One status vocabulary in the list and the session subtitle (`SessionStatus`): **Working** (created, starting, running; spinner), **Needs approval** (orange), **Failed** (red), **Your turn** (idle: the agent finished its turn; tint dot), **Ended** (grey). The session subtitle leaves out **Your turn**, showing only the branch or login.
- **Archive**: swipe a session (or its context menu, or the Session "…" menu). A live session asks first, because `session.archive` ends it; an ended one is archived at once. Archived sessions leave the list and are no longer re-subscribed; `session.changed` with `archived_at` set or cleared moves them out of or back into the list. Settings → Archived Sessions lists them (`session.list {archived: true}`, loaded on demand, not cached), with search; a transcript opens read only with Unarchive (`session.unarchive`). There is no Delete: the host's event log is append-only.
- **Default login per workspace** (`Workspace.default_agent` / `default_profile_id`, set by the host on a workspace's first session): New Session preselects it and marks it "Default"; picking another login offers "Make default for <workspace>", which calls `workspace.set_default_agent` after the session starts. A login tapped in the sheet survives the sheet's refresh and its automatic Quick Task choice; only tapping another workspace applies that workspace's default. Workspaces are refreshed after every new session.

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
  Enclave behaviour, the camera scanner, photo/file pickers, audio
  recording, layout, and all of push notifications on the phone (APNs
  registration, the Notification Service Extension, the shared Keychain
  group, the entitlements and the provisioning-profile check).
