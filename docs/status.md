# Status against the v1 spec

As of 15 November 2026, on the spec author's Linux machine (Arch, kernel 7.2,
Node 26, bubblewrap, `sandbox-runtime` 0.0.83, Codex via `codex-acp` 2.6.0
bundling Codex 0.166.0, `claude-agent-acp` 0.107.0, OpenCode 2.3.0).

**Legend:** ✅ verified (how is stated) · 🟡 partly verified · ❌ not verified · ⛔ not built

Nothing here was run on macOS. The macOS code paths (Seatbelt through
`sandbox-runtime`, Keychain secrets) are written but **not compiled or run**.

## Milestones

| # | Milestone | State |
| --- | --- | --- |
| 0 | Finish the spike | 🟡 Linux answered (below); macOS and the nested-sandbox question not run |
| 1 | Host core | ✅ `cargo test` (store, state machine, protocol, resume, idempotency); CLI ran full sessions against the fake agent through the daemon |
| 2 | First agent (Codex) | ✅ real Codex sessions from the CLI: prompt, tool call, cancel, MCP. Run sandboxed, never unsandboxed |
| 3 | Sandbox | 🟡 13-check escape suite green on Linux (`tests/m3_escape.rs`); CI defined for Linux and macOS but not run; macOS ❌ |
| 4 | All agents | 🟡 all three complete sandboxed turns with tool calls on Linux; checklist partly covered (below) |
| 5 | MCP gateway | 🟡 gateway checklist against a test server and a real public server (DeepWiki); OAuth sign-in written but not run against a real OAuth server |
| 6 | Remote access | ✅ CLI pairs and runs sessions through the relay; tampering, replay and revocation tests pass; checked by hand against the Worker deployed on Cloudflare (16 Nov 2026) |
| 7 | iOS app | 🟡 Rust FFI (`crates/ffi`, UniFFI) ✅ tested end to end (pair, connect, stream, revoke, host offline) and through generated foreign bindings; SwiftUI app (`ios/`, six screens, Face ID gate, Secure Enclave–wrapped Keychain keys, switcher cover, offline cache) **never compiled** — needs a Mac |
| 8 | Desktop app | 🟡 seven screens in Rune (`desktop/`); every screen rendered headless and checked by eye; model and bridge tested against a real host; not run as an interactive window, not run on macOS |

## Chat features (17 Nov 2026)

| Feature | Status |
| --- | --- |
| Markdown in agent replies | Desktop ✅ (rune-kit `Markdown`, streaming mode for the live reply; links open only for http/https/mailto), rendered headless and checked by eye. iOS: block parser (`ios/AgentHost/Core/MarkdownBlocks.swift`) passes 16 tests in a Linux Swift 6 container; the SwiftUI view **not compiled** |
| Attachments | Host ✅: `upload.begin/append/finish` (512 KB pieces, 25 MB a file, 10 a message), files kept in `<data>/uploads` (owner-only, outside every sandbox), copied into the session's temp folder at turn start and named in the prompt text; images go inline when the agent takes images, small text files embedded when it takes embedded context, otherwise `resource_link`. Tested with the fake agent over the socket and through the relay (FFI), and with **Cursor** in the sandbox (it read the attached file). Desktop: file picker, drop (not on Wayland: winit has no file drags there), chips, names in the prompt bubble; rendered headless. iOS: written, **not compiled**. **Sent images again (protocol 7):** a prompt's images move to `<data>/attachments/<session>/<id>` when it is recorded (kept as long as the session; other files are not kept), the agent reads them from there, and `attachment.read` (`session_id`, `attachment_id`, `offset`; 512 KB pieces) gives them back. Host ✅ in process (`tests/attachments_core.rs`: an image alone reaches the agent, a retried prompt, reading back in pieces, other sessions and paths refused). Desktop: thumbnails above the bubble (none for a picture alone), a click opens a full-window viewer, Esc closes; model tests and a headless click-through pass, not rendered to pixels. iOS: pictures under the prompt (own images cached as they upload, others fetched and cached in `AgentHostCache/images`), a full-screen viewer with zoom and Share, nano frames; **not compiled** |
| Live updates and reconnects | Host streams reply chunks live over the socket and through the deployed relay (timed with Cursor and Claude Code: chunks every 20–40 ms). Devices ping the relay every 15 s and treat 40 s of silence as a dead link (tested with a relay that goes silent; an idle link to the deployed relay stays up); the host does the same towards the relay (20 s / 50 s). Both apps re-subscribe a session when the host reports newer events than they hold and they don't arrive within 1.5 s (desktop: tested by dropping the stream; fails without it). iOS reconnects at once when the network switches and every few seconds while the host is offline. iOS changes **not compiled** |
| Quick tasks (no project folder) | Host ✅: built-in `ws_quick` workspace (kind `scratch`), a fresh owner-only folder per session next to the data folder (hidden from every other session), several at once; can't be removed. New `web` network preset (any public host; private and local addresses still refused by the proxy, every connection logged), on for Quick tasks by default, a switch on every workspace. Tested with the fake agent and unit tests; **no real agent has looked anything up through it yet**, and quick tasks' sandbox (a writable folder inside a hidden one, like each session's temp folder) is as untested on macOS as the rest. Desktop ✅ (Workspaces: "Open web" switch, Quick tasks built in). iOS: listed first on New session; builds, not run |
| Projects from the phone | Host ✅: `project.browse` / `project.create` (phones may call them) only inside the project folders, default `~/Code`, set with `project.set_roots` (admin only); hidden folders and symlinks out of a project folder are skipped; the usual workspace checks apply. Tested as a phone connection. CLI ✅ `agenthost project …`. Desktop ✅ "Project folders" card. iOS: "New Project…" on New session (browse, use or create a folder); builds for the simulator, not run |
| Session statuses, archive, per-workspace default login | Host ✅: `session.archive` (ends a live session, hides it from `session.list`; `archived: true` lists them) and `session.unarchive`; workspaces remember a default login, set by their first session and changed with `workspace.set_default_agent` (phones may call all three). Tested with the fake agent, including archive surviving a restart. CLI ✅ (`session archive/unarchive`, `session list --archived`, `workspace default`). Desktop and iOS: statuses (working, needs you, failed, your turn, ended), archive actions, an Archived list in Settings, and the default login preselected in New Session |
| Pinned sessions, compact desktop sidebar | Host ✅: `session.pin` (`pinned: true/false`, phones may call it) sets `Session.pinned_at` (protocol 8); pinning again keeps the first time, so a pin keeps its place. Store test passes; the socket test (`tests/archive_and_defaults.rs`) is **written but not run here**. Desktop (nano page "Desktop sidebar v3 (compact)"): one-line rows (status, title, pull request and automation icons, age up to weeks and months) with a second line only for needs you, failed or agents running; the login, branch and PR number in the tooltip; New Session and Automations as rows; search behind a button (⌘F / Ctrl+F, Escape clears then closes); a Pinned section; collapsible workspaces (saved in `desktop.json` in the config folder) with an always-visible "+"; the newest 8 per workspace and "Show N more"; Pin and Archive on hover and in the row menu (hosts on 8); ⌘1…9 / Alt+1…9 shown while the modifier is held; grey selection on macOS; the GNOME header matches macOS (toggle and search at its end). Pure model tests and a headless click-through in both looks pass; host-backed test written but not run here; not rendered to pixels (no GPU here). iOS: not yet (it ignores the new field) |
| Agent profiles (several logins per agent) | Host ✅: `agent.profile_add/remove/list`, one `host.info` entry per login, `session.create` takes `profile_id`; the folder goes in the agent's own setting (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, OpenCode `XDG_DATA_HOME`, Cursor `XDG_CONFIG_HOME` on Linux; read from each CLI's code; none yet for Antigravity, whose folder is the `--gemini-dir` flag). Folders are checked against home, secrets, host data, other logins and workspaces; each session's sandbox hides every other login. Tested with the fake agent (state lands in the profile folder, resume of a removed profile fails) and the policy unit tests; **not tried with a real second subscription**. CLI ✅ (`agenthost agent …`, `claude:work`). Desktop ✅ choose on New session, add/remove on Host status (rendered headless). iOS: choose on New session; builds for the simulator, not run |
| Installing tools | Host ✅: the gateway's built-in `agenthost__install_tool` (npm, pipx, uv, cargo, brew, go; offered only for managers found on PATH) always asks first, shows the exact command, the agent's reason and "runs as you, outside the sandbox", and only then runs it, as an argument list with no shell, one at a time, with a 15-minute limit; registry package names and plain versions only (no URLs, paths, git specs, flags or ranges); decisions and results go to the audit log. Each session can also write a shared session tools folder next to the data folder (npm prefix, pip user base, pipx, uv, cargo and go homes, their `bin` first on PATH), so agents install helpers for themselves without approval and without touching your home folder. Tested with a fake package manager (denied runs nothing, approved runs exactly what was shown) and unit tests; **no real package manager or real agent has used it yet**. Desktop ✅ install card (rendered headless in both looks). iOS: install card and approval sheet; builds, checked in the simulator demo |
| Model and effort | Host ✅: the agent's ACP `configOptions` (select kind; `mode` left out, the host sets it) in `Session.config_options`, changed with `session.set_config`; the last choice per login is applied to new sessions. Tested with the fake agent; **not tried with the real Claude adapter** (shapes read from its source). Desktop and iOS: model and effort pickers in the composer; rendered with demo data |
| Side questions (`/btw`) | Host ✅: `session.side_ask` forks the conversation (ACP `session/fork`, offered when the agent advertises it) and answers alongside a running turn; the fork's output never reaches the main transcript (`side.*` events); forks get no connectors. Tested with the fake agent; **not tried with a real agent**. Desktop: side panel with a tab per question; iOS: a sheet. Rendered with demo data |
| Connector catalog and icons | Host ✅: `connector.search` queries integrations.sh (MCP servers only, icons as `data:` URLs); each connector gets its site's favicon (Google's favicon service, so the domain is sent there) when added. Rename refuses a tool prefix that clashes. Tested against a local server; **the real services were not called by the tests**. Desktop: search when adding, rename, icons (bearer tokens no longer offered). iOS: icons on approvals |
| Pull requests | Host ✅: `agenthost__create_pull_request` (offered when `gh` is on the host's PATH) shows the title, description, base and head, and after approval runs `gh pr create` as you outside the sandbox (description on stdin, no prompts; git commands gh runs get hooks and fsmonitor off through git's environment config). Tested with a fake `gh`; **not tried against GitHub**. Desktop and iOS show the description on the approval |
| Running commands as you | Host ✅: the gateway's built-in `agenthost__run_command` takes an argument list (no shell), optional stdin and a reason; the program must be a bare name on the host's own PATH, never in the session tools, worktrees or scratch folders or the session's folder. The approval shows the exact command (quoted as a shell would read it), the input and the reason; after approval the host runs it as you in the session's folder (no prompts, git hooks and fsmonitor off, 10-minute limit, output tail kept) and logs it to the audit log. "Always Allow" remembers a prefix of the program and up to two subcommand words (`gh pr edit`) through `builtin.set_policy {name: "run_command", command}` (desktop only); never offered or accepted for a bare program, shells, interpreters, wrappers (`env`, `sudo`, `xargs`), package managers or build tools, or git. Unit tests for planning, rules and running; an integration test with a fake `gh` (`tests/run_command.rs`) written but **not run here** (needs a writable `/tmp`); **not tried against GitHub**. Desktop: approval card with "Always Allow “gh pr edit”", a toast, and Settings → Connectors → Built-in tools lists the allowed prefixes with Ask First (pure model tests pass; not rendered headless here). iOS: approval sheet labels the input; **not compiled** |
| Pushing and fetching | Host ✅: the gateway's built-in `agenthost__git_push` asks first (exact command, the agent's reason, a separate "Force-Push" label for `--force-with-lease`), `agenthost__git_fetch` runs without asking; both run git outside the sandbox with your credentials, in the session's worktree or workspace, only against remotes the repository has and branch names (no refspecs, deletes or URLs), with hooks and fsmonitor off and a 5-minute limit; logged to the audit log. Tested with a local bare remote (denied pushes nothing, approved pushes the branch, a planted `pre-push` hook doesn't run); **not tried against GitHub with real credentials**. Desktop and iOS: the built-in approval card takes its button label and warning from the host |
| Screen sharing (POC) | Host ✅ with a fake engine: `screen.status/start/stop/attach/set_policy` (protocol 3). Off until the desktop allows it (`screen.set_policy`, admin only), watching and control allowed separately; `screen.start` starts [screen-relay](https://github.com/vercel-labs/screen-relay) on loopback (30 fps, 0.5–8 Mbps) and returns a one-time ticket; the phone redeems it as the first message of a second tunnel, which then carries screen-relay's `nsl1` messages untouched (ACKs always, input only with control; one viewer at a time, a new one replaces the old; a viewer is ended after 15 minutes; stops 60 s after the last viewer). Modes: your desktop (macOS main display; X11; Wayland compositors with the wlroots protocols, not GNOME or KDE) and, on Linux, a browser in a separate virtual desktop. Monitors: on Hyprland the host lists them over Hyprland's IPC socket (no hyprctl or `HYPRLAND_INSTANCE_SIGNATURE` needed; the instance whose lock names our Wayland socket wins) in `screen.status` (`outputs`); `screen.start` takes `output` and restarts screen-relay with `--output` when it changes; other Wayland compositors take a name without a list. Browser mode starts the browser on X11 (`--ozone-platform=x11`, no `WAYLAND_DISPLAY`). iOS: Monitor section in the picker and a Monitor picker in the viewer's ••• menu that switches in place (never compiled). **Monitor listing not run against a real Hyprland** (the sandbox blocks Unix sockets). Tested through the dev relay against a fake engine (`tests/screen.rs`: policy, view-only drops input, control reaches the engine, tickets work once, takeover, idle stop). Against a real engine ✅: screen-relay 0.1.2 in Docker (X11 virtual desktop with Chromium), host on macOS, through the dev relay, `agenthost --remote … screen record --click … --type …`: 37 H.264 frames at 1280×800 arrived and decoded, and the click and the typed text reached the page (19 Nov 2026). **Not run on a Mac's own display, on Hyprland, with a locked screen, or from a real iPhone.** `agenthostd install-tools` installs screen-relay 0.1.2 next to the adapters and builds its engine. CLI ✅ `agenthost screen status|allow|off|stop|record`. Desktop: Settings → Devices → Screen sharing (rendered headless in both looks). iOS: Host Screen (Settings, and the sessions toolbar when allowed): mode picker, landscape while viewing, `AVSampleBufferDisplayLayer` H.264 viewer with tap/hold/drag/pinch and a key bar; decodes a real H.264 clip in the simulator demo; never run against a host |
| Push notifications | **Through the relay (protocol 6), the default:** the relay of whoever signs the app holds that team's APNs key as Worker secrets (`APNS_KEY`, `APNS_KEY_ID`, `APNS_TEAM_ID`, `APNS_TOPIC`), so hosts need none. The phone registers its token with the relay its build names (`AgentHostPushRelay`, else the relay it is paired through; `POST /v1/push/register`) and gets a handle (the token, AES-256-GCM-sealed by the relay, bound to one host's relay id); the host stores only the handle and posts each notification to `POST /v1/push/{relay_id}/send`, signed with its relay key (Ed25519 over the relay id, time and body hash; 5 minutes' clock skew); the relay checks it, allows 60 a minute per host, adds the generic alert and sends to APNs (`BadDeviceToken` retried in the other environment), storing nothing. Relay ✅ 10 vitest tests against an APNs stand-in (provider token signature, topic, payload; wrong host, tampered or stale signatures, rate limit, `Unregistered`, environment switch) and a Rust signature vector; host ✅ in process against a relay stand-in (`push::tests::delivery::a_relay_sends_for_phones_so_the_host_needs_no_key`). A relay without the secrets answers 404 and the phone falls back to the host's own key; the phone's Settings → Notifications then shows the key section and says nothing is sent until there is a key. **Not tried against Apple's APNs, the deployed relay or a real iPhone; iOS not compiled.** **With the host's own key (builds no relay sends for):** host ✅ in process: `push.status/set_key/register/unregister/set_prefs/test` (protocol 4) and `push.clear_key` (desktop only). The host sends to APNs itself over HTTP/2 with an ES256 provider token from the account's `.p8` key (kept in the secret store; a phone may hand it over while pairing, or `agenthost push setup`). Each phone registers its token, bundle id, APNs environment and a 32-byte key; a notification's title and body (the session, the approval's summary or tool, the reply's first words, why it failed) are sealed with that key (ChaCha20-Poly1305, `nonce‖ciphertext‖tag`, as CryptoKit reads it) and Apple sees only "A session needs you". Approvals on by default; finished turns (not cancelled ones) and failures when the phone asks. A token APNs refuses as `BadDeviceToken` is tried in the other environment and the one that works is kept; `Unregistered` drops it; revoking a device drops it. Tested against an APNs stand-in (HTTP/2, the token's signature checked) with a real host's store and event bus (`push::tests::delivery`); the end-to-end test through the relay with the fake agent (`tests/push.rs`) is **written but not run** (the machine it was written on could not open Unix sockets). **Not tried against Apple's APNs or with a real iPhone.** CLI ✅ `agenthost push status|setup|test|remove-key`. iOS: Notifications step after pairing (with the APNs key from Files), Settings → Notifications, a notification service extension that opens the content; **not compiled** |
| Session titles | Host ✅: a session the user hasn't named is named by the host, so every device shows the same name. Its first message, shortened, names it straight away; the agent is then asked for a 2–6 word title in a hidden conversation of its own (no tools or connectors, on its quickest model where it has one, e.g. Haiku), and a title the agent gives the session itself (ACP `session_info_update`) wins over both. A title from `session.rename` always shows over the host's; clearing it brings the host's back. Older sessions are named from their first message when the host starts. Tested with the fake agent (`tests/session_titles.rs`, **written but not run**: the machine it was written on could not open its temp folder). **Not tried with the real agents.** Desktop and iOS show the title everywhere, "New session" before the first message; iOS **not compiled** |
| Voice input | Host ✅: NVIDIA Nemotron 3.5 ASR streaming 0.6B (int8, sherpa-onnx export, 475 MB) downloaded on request with a pinned SHA-256, run on the CPU through `sherpa-onnx` 2.0.1 (statically linked ONNX Runtime, about 36 MB in `agenthostd`). Download, checksum, unpacking and transcription through `voice.*` checked here (sample French and German clips: about 0.5–0.8 s for 3–5 s of audio after a 1.5 s first load). Live dictation (`voice.stream_begin/append/finish`): the nine sample clips fed in 200 ms pieces give words before the clip ends (first words after 0.8–1.8 s), each piece decodes in about 110–130 ms, finishing takes about 200 ms, and the final text matches transcribing the whole clip. Desktop: words appear in the composer as you talk; Return stops the microphone and sends once the last words are in, Esc discards; older hosts fall back to record-then-transcribe. Microphone through `cpal` checked (records a WAV); the full speak → text path not tried with a real voice. iOS: uses iOS dictation (Speech framework: `SpeechTranscriber` on iOS 26, `SFSpeechRecognizer` before; on device where supported, with workspace names and developer terms as hints) instead of the host model, so nothing is downloaded for the phone; builds for the simulator, not tried with a voice |
| Automations (schedules, webhooks) | Host ✅ in process: `automation.list/get/create/update/delete/run_now/runs/check_schedule` and `automation.changed` (protocol 5). An automation is a saved session (workspace, login, prompt, time limit) with a trigger: a five-field cron in the host's local time (libc `localtime_r`, so DST is the system's; a minute that repeats when clocks go back runs once; at most every 5 minutes), a webhook, or by hand. Each run is a session the host creates (`Session.automation_id`), prompted once and watched until its turn ends, fails or passes `max_minutes` (then cancelled); the run records its outcome, the agent's last reply and why it ran. The newest run's session stays open; the previous one is ended when the next run starts. A scheduled run is skipped while the previous one is going; a time missed by up to 12 h (off or asleep; the scheduler checks the wall clock every 30 s) runs once as a catch-up, older ones are recorded as skipped; runs going when the host stopped are marked failed at start. Pre-approved tools (admin only): `create_pull_request`, and `git_push` of the run's own branch only (a force-push still asks); approvals in runs wait 12 h. Phones may manage schedule and by-hand automations but not webhook triggers, secrets or pre-approvals (checked per field on the host). Webhooks: the relay seals each POST to the host's X25519 hook key (HKDF-SHA256, AES-256-GCM, bound to the delivery id), queues it up to a day and answers 202 at once; the host opens it, drops repeats by delivery id, checks Sentry's `Sentry-Hook-Signature` / GitHub's `X-Hub-Signature-256` (HMAC-SHA256) or a bearer token, applies the source's rules (Sentry: issue alerts, new issues, metric alerts but not resolved ones; shapes from Sentry's docs) and filters, dedupes (Sentry: per issue or metric alert, a day by default; deliveries are serialized so a burst can't slip through), fills `{{…}}` fields (one line, 300 characters each), adds an untrusted-data note, attaches the payload as `webhook.json` and starts a run. Tested: cron (5 unit tests), webhook verification and templates (5), the Worker's sealed blob opened in Rust (cross-language vector), and runs with the fake agent driven in process (`tests/automations_core.rs`: run now, schedule, catch-up, skip, restart, Sentry deliveries, dedupe, a burst of five). The socket and dev-relay tests (`tests/automations.rs`: the API end to end, a signed Sentry alert POSTed over HTTP to the dev relay, pre-approved pushes) are **written but not run here** (the machine they were written on can't open Unix sockets). Relay ✅ 18 vitest tests (ingress, sealing, queue, ack, expiry, limits). **Not tried against Sentry, GitHub or the deployed relay, or with a real agent.** CLI ✅ `agenthost automation ls|show|add|edit|run|runs|rm|check`. Desktop: Automations in the sidebar (and View menu): list with on/off, detail with Run Now, run history (a run opens its session), webhook URL, copy, new URL, last delivery and setup hints; editor with schedule presets checked live against the host, webhook sender and secret, dedupe and filters, pre-approval switches; a clock on run sessions. Pure model tests and a headless click-through pass; host-backed tests not run here; not rendered to pixels (no GPU here). iOS: Settings → Automations (protocol 5 hosts): list, detail with runs, editor (schedule and by hand; webhooks and pre-approvals read-only on the phone), an Automations notification switch, nano frames; **never compiled** |
| Notes (ideas and todos for later) | Host ✅ in process: `note.list/create/update/delete` and `note.changed` (protocol 9); phones may call all of them (a note only becomes a prompt when you start a session from it). A note belongs to a workspace (its list skips removed workspaces; at most 1,000 per workspace, 20,000 characters each), is plain text (the first line is the title) and is open or done (`done_at`). `session.create` takes `note_id` (same workspace only): the note lists the session (`note_sessions` table, so `Session` is unchanged) and opens again if it was done; archiving a session started from a note marks its open notes done; unarchiving leaves them. Tested with the fake agent in process (`tests/notes_core.rs`: validation, order, notifications, links, archive marks done). CLI ✅ `agenthost note ls|add|edit|done|reopen|rm`. Desktop: Notes in the sidebar (open count) and main menu, New Note Here on a workspace; notes by workspace beside an editor (⌘↩ / Ctrl+Enter saves; switching notes saves too; unsaved edits win over changes from a phone), Mark Done / Reopen, Delete (asks), the sessions started from it, Start Session (the New Session sheet in its workspace, then the note in the new session's composer). Pure model tests and a headless click-through in both looks pass; not rendered to pixels (no GPU here). iOS: a note button in the session list (protocol 9 hosts), notes by workspace with Show Done, swipe Start Session / Delete, an editor that saves as you type, New Note (also from a workspace's long-press menu), New Session from a note (its text in the composer), nano frames "iOS — Notes", "iOS — Note", "iOS — New note (sheet)"; **never compiled** |

## Milestone 0 answers (Linux)

| Question | Answer |
| --- | --- |
| Claude Code completes a turn in the sandbox | ✅ yes, with shell tool calls and an MCP call through the gateway |
| OpenCode completes a turn in the sandbox | ✅ yes, with shell tool calls and an MCP call through the gateway |
| Full ACP session (prompt, tool call, resume) | ✅ for Claude Code (prompt, tool call, cancel, resume after host restart with history intact); Codex and OpenCode: prompt, tool call, MCP; resume ❌ not tried |
| Agents' built-in sandboxes nested in ours | ❌ not tested. Codex's is switched off (ACP mode `agent-full-access`); Claude Code's and OpenCode's are not used. The host's own sandbox is the boundary |
| Hosts needed for a turn | Codex: `chatgpt.com` ✅. Claude Code: `api.anthropic.com` sufficed (also allowed: `console.anthropic.com`, `claude.ai`, `platform.claude.com`). OpenCode: `opencode.ai`/`models.dev` sufficed for this account's model; it also tries `auth.openai.com` (refused) for an OpenAI login it holds |
| Cursor completes a turn in the sandbox | ✅ yes (CLI 2026.11.13), with an MCP call through the gateway; `~/.cursor` and `~/.ssh` were unreadable from inside. Resume ❌ not tried |
| Antigravity CLI completes a turn in the sandbox | ❌ not tried: no Google login on the test machine, and agy was not run in a session. Flags read from `agy acp --help` (1.5.14); folders, hosts and the keyring fallback read from the 1.5.14 binary's strings and changelog |
| Anything on macOS | ❌ |

## Agent checklist (per agent, Linux)

| Outcome | Codex | Claude Code | OpenCode |
| --- | --- | --- | --- |
| Start and reach `idle` in < 10 s | ✅ observed | ✅ observed | ✅ observed |
| Reply streams as events | ✅ | ✅ | ✅ |
| Shell command + file edit in the workspace without a prompt | ✅ | ✅ | ✅ |
| Write outside the workspace fails, visible in the log | ✅ | ✅ | ❌ not tried |
| Unlisted host fails with `net.denied` | ✅ | ✅ (its own extra hosts) | ✅ (its own extra hosts) |
| Cancel → `idle` within 5 s | ❌ not timed | ✅ | ❌ |
| Host restart, resume with history | ❌ | ✅ | ❌ |
| Logged out → `failed`, "not logged in", fix command | 🟡 logic tested with the fake agent only (not logging you out) | same | same |
| Two sessions, different agents, at once | 🟡 tested with the fake agent | | |

## Sandbox checklist

| Outcome | State |
| --- | --- |
| Escape tests on Linux and macOS | 🟡 Linux ✅ (13 checks: secrets folders, other agents' credentials, host data, writes outside, unlisted host, proxy bypass, host loopback, private addresses even when listed, DNS, other sessions' temp, inbound listeners, host socket; plus writes inside work). macOS ❌ |
| Same policy, same results on both | ❌ |
| `agent-only` blocks the npm registry; switching to `packages` works without restarting | ✅ (`profile_change_applies_without_restart`; applies even inside a running session) |
| Denied hosts and writes in the event log | ✅ hosts as `net.denied`; writes as failed `tool.result` flagged `sandbox_denied` (heuristic on the error text) |

## MCP gateway checklist

| Outcome | State |
| --- | --- |
| Add a connector with OAuth from the desktop, tools labelled read/write | 🟡 adding, listing and labelling ✅; OAuth (discovery, dynamic registration, PKCE, refresh) implemented, ❌ not run against a real OAuth server |
| Read tool with no prompt | ✅ test server; ✅ DeepWiki with Claude Code, Codex and OpenCode |
| Write tool pauses the session; approval on every device within 2 s | ✅ test (two clients, < 2 s); ✅ Claude Code + DeepWiki approved from a second CLI |
| Approving on one device clears it on the other | ✅ `approval.resolved` goes to every subscriber; answering twice fails |
| Connector token findable in the sandbox | ✅ not found in the agent's environment, files, the event log or the database |
| Disabling a connector removes its tools at once | ✅ (even in a running session) |

## Client protocol checklist

| Outcome | State |
| --- | --- |
| CLI can do everything the desktop can | ✅ every method has a CLI command (`agenthost raw` for anything else) |
| Kill a connection mid-turn, reconnect: no gaps, no duplicates | ✅ test |
| Phone calling a desktop-only method: capability error, logged | ✅ test (audit log at `data/logs/audit.jsonl`) |
| Old protocol version: "update required" | ✅ test |

## Remote access checklist

| Outcome | State |
| --- | --- |
| Pairing in < 30 s | ✅ (about a second with the dev relay; a few seconds through `wrangler dev` and the deployed Worker) |
| QR after 2 minutes, or a second time, refused | 🟡 second use ✅ (relay 4403); expiry implemented on host and relay, ❌ not waited out in a test |
| A logging relay yields no readable content | ✅ dev relay frame log checked for plaintext; Worker logs only role, relay id and close codes |
| Replayed or altered frames close the connection, state intact | ✅ tests |
| Revoked device disconnected in < 2 s and cannot reconnect | ✅ dev relay test; ✅ by hand through the deployed Worker: a connected remote CLI was dropped 0.2 s after revoke, later connections refused (4403) |
| No open network ports | ✅ `ss -ltnp` shows none for an idle `agenthostd`; see deviations for per-session loopback ports |
| The desktop app pairs as a device | ✅ model and bridge tests through the dev relay: paste the payload, compare the 6-digit code, connect, run a session over the tunnel, an admin method refused (`workspace.add`), revoked once and not retried; a computer with its own host in use is not switched by pairing. Settings pages render read-only with a note (checked headless in both looks). ❌ not tried against the deployed Worker |

## Deployed relay check (16 Nov 2026)

Against the Worker deployed on Cloudflare, with a scratch host and the fake
agent: the host connects; the CLI pairs as a device (codes matched on both
sides); `status` and a sandboxed session run through the tunnel in about a
second; the device is refused `device.revoke` (capability error); a revoke
from the host drops its open connection in 0.2 s and every later connection
is refused. The host listened on no TCP port throughout.

## Decisions made while building (worth reviewing)

- **A paired desktop stays read-only.** The desktop app can pair with a remote
  host through the relay like the CLI and the phone, but a paired connection
  holds phone capabilities on purpose: sessions and approvals, never policy.
  Admin methods stay reachable only from the host's own socket, so keys
  lifted from a laptop (a 0600 `remote.json`) cannot reconfigure the host,
  and `device.revoke` ends them. If full remote management is ever wanted,
  the pairing protocol grows a device kind and the host grants it
  deliberately at pair-confirm — `PairHello` and that prompt are the only
  places that would change.
- **Egress proxy listens on loopback.** `sandbox-runtime` bridges the sandbox to
  a TCP port on the host, so each live session has two ephemeral
  `127.0.0.1` ports. On Linux the proxy refuses connections from sockets owned
  by other users (checked in `/proc/net/tcp`). They close with the session.
- **OAuth sign-in opens a loopback listener** on `127.0.0.1` for up to five
  minutes to receive the browser redirect.
- **Several live sessions per workspace.** In a git repository each session
  gets a worktree under `<data>-worktrees/<session>` on its own branch
  (`agenthost/<id>`), made by the host from `HEAD`; the session works there
  (in the same subfolder when the workspace is one), your checkout stays
  read-only to it, and other sessions' worktrees are unreadable. Inside the
  sandbox the repository's `.git` is writable (commits, branches, fetched
  refs, `packed-refs`), except what would run something outside the sandbox
  or point your checkout elsewhere: `config`, `config.worktree`, `hooks`,
  `commondir` and `modules`, and the worktree's `.git` pointer. On Linux,
  where only existing paths can be protected, the host creates a `commondir`
  of "." (the repository itself) and empty `hooks` and `modules` folders. Archiving removes a worktree without uncommitted changes
  (the branch stays); unarchiving or resuming brings it back from the branch.
  Outside git (or in a repository without commits) sessions share the folder;
  on Linux the host creates `sandbox-runtime`'s read-only placeholders itself
  and removes them when the last session there ends, because `sandbox-runtime`
  leaves placeholders it did not create in place. Sessions created before
  this keep working in the workspace folder. The host's own git commands run
  with hooks and fsmonitor off, so nothing from the repository runs outside
  the sandbox.
- **Host-controlled agent configuration**, as found necessary:
  - Codex: a per-session wrapper (`CODEX_PATH`) plus `CODEX_CONFIG` turn off
    every MCP server in your `~/.codex/config.toml` and the workspace's
    `.codex/config.toml` by name (Codex merges overrides; only names are read),
    and the `hooks`, `plugins`, `remote_plugin`, `apps`, browser and
    computer-use features. ACP mode `agent-full-access`.
  - Claude Code: SDK options `settingSources: []`, `strictMcpConfig: true`;
    `ENABLE_CLAUDEAI_MCP_SERVERS=false`, no marketplace auto-install,
    updates or telemetry.
  - OpenCode: `~/.config/opencode` is hidden inside its sandbox and a host
    config is supplied via `XDG_CONFIG_HOME`; project config and Claude Code
    compatibility off.
  - Cursor: `~/.cursor` (your MCP servers, hooks, plugins, skills) is hidden;
    `CURSOR_CONFIG_DIR`/`CURSOR_DATA_DIR` point at `~/.local/share/agenthost-cursor`;
    the login in `~/.config/cursor` stays readable. `--sandbox disabled`,
    ACP `authenticate` with `cursor_login`.
  - Antigravity CLI (`agy acp --disable-telemetry`, its own `--sandbox` off):
    only `~/.gemini/antigravity-cli` (settings, logs, the file login) and
    `~/.gemini/antigravity` (conversations) are writable; `~/.gemini/config`
    (plugins, `mcp_config.json`, hooks, skills, rules), `~/.gemini/skills`,
    `extensions` and `commands` are hidden. `antigravity-cli/hooks.json` sits
    in a writable folder, so it can't be hidden.
- **Gateway without the proxy.** Cursor's MCP client ignores `HTTP_PROXY`, so
  Cursor and Antigravity get the gateway at the proxy's own address
  (`http://127.0.0.1:3128/s/<token>/mcp` on Linux, which the sandbox runtime
  bridges to the session proxy and excludes via `NO_PROXY`); the proxy sends
  origin-form `/s/…` requests to the gateway.
- **Streaming.** Chunks go to clients as ephemeral `session.delta`
  notifications; the coalesced text is stored as one `agent.message` when the
  text ends (before a tool call, or at turn end).
- **Secrets.** Secret Service (via `secret-tool`, secrets on stdin) if one
  answers, else owner-only files under `data/secrets`; macOS Keychain. The
  choice is recorded on first start. This settles the spec's milestone 1 open
  question.
- **Relay identity.** A host's relay id is derived from its Ed25519 relay key,
  so ids cannot be claimed. The relay pairing token is derived from the QR
  secret, so the relay never learns the Noise PSK.
- **Extra protocol methods** beyond the spec's table: `host.check`,
  `approval.list`, `connector.sign_in`, `connector.refresh`,
  `device.pair_confirm`, `device.rename`, `events.unsubscribe`;
  `session.list {watch:true}` gives the snapshot-then-changes stream.
- **Repeated failure guard.** The same failing tool call three times in one
  turn cancels the turn; an agent that ignores cancel for 3 s is restarted
  and resumed.

- **A session's changes and files, read only** (25 Nov 2026, protocol 11).
  `session.changes` is `git diff` from where the session's branch left the
  remote's main branch (`merge-base` with `origin/HEAD`, else `origin/main`
  or `origin/master`), in its worktree, so uncommitted work counts. The host
  fetches that branch first (at most once a minute per repository, 15 s at
  most; offline, what was fetched last stands), so a local `main` that is
  behind doesn't matter; a repository without a remote compares with the
  branch the workspace has checked out. Each untracked file
  is added to the patch as an added one; an archived session
  whose worktree is gone gets its branch's commits. `session.files` is what
  git tracks plus what it doesn't ignore (every file for a folder outside
  git), `session.file_read` one file's first megabyte. Git runs as it does
  for worktrees (no hooks) with external diff and textconv off; a path or a
  link that leaves the session's folder is refused. All three need only the
  sessions capability, so paired devices can use them: the agent can already
  print any of this in the conversation. The desktop draws them with
  rune-kit's diff view, tree and highlighter. Tested on Linux in process
  (`tests/files_core.rs` against a real repository; the desktop's model and
  both tabs laid out and clicked through headless in both looks). Not yet
  seen as pixels, not run on macOS, and not joined end to end to a live host
  session. Not built: review comments on diff lines, images, search inside a
  file, the iOS app.

- **Terminals are the host's, and local only** (25 Nov 2026, protocol 10).
  The desktop's terminal is a shell `agenthostd` runs in a pseudo-terminal it
  owns; the app is only the emulator (`alacritty_terminal` drawn with Rune's
  `grid`). The host keeps the last megabyte of output to replay, and stops
  reading the shell while an attached client is 512 KB behind its last
  acknowledgement (a client quiet for 2 s no longer holds it up). Every
  `terminal.*` method is admin only, so paired devices get none; letting a
  phone in would need a policy like screen sharing's. Tested on Linux in
  process: the host's API against a real `sh` (`tests/terminal_core.rs`:
  run, resize, replay after reconnecting, a 3 MB flood, refusal for a phone),
  the app's emulator and keys, the panel laid out and typed into headless in
  both looks, and the app's model joined to a real host and shell. Not yet
  seen as pixels or typed into by hand, and not run on macOS. Known gaps: no
  mouse reporting to programs, no input methods (IME), box-drawing characters
  drawn from the font (rows may show seams), and no inline images.

## Known issues

- `agenthost-cli::remote::connect` reports why a tunnel closed only as text; the FFI parses the code out of it.
- The iOS library pulls in the CLI's dependencies (clap, qrcode); splitting `Client`/`remote` into a lib-only crate would slim it.
- Unpair on the phone is local only (a phone may not call `device.revoke`); revoke it on the desktop too.

- Codex turns hung twice (no output from Codex at all, no denied hosts). Not
  reproducible on retry. Cancel or end recovers.
- `codex-acp` 2.6.0 bundles Codex 0.166.0, not the 0.169.0 the spike tested.
  Setting `CODEX_PATH` to another binary is possible but not wired.
- OpenCode is found on `PATH`; the first `opencode` here was 1.2.22, so the
  host picks the first 2.x it finds (override with `AGENTHOST_OPENCODE_BIN`).
- Codex can still read its own stored MCP OAuth tokens in `~/.codex` (its own
  state path); the proxy blocks their hosts.
- On macOS, Claude Code's login check is a guess (`~/.claude.json` exists).
- Cursor's blocking extension requests (`cursor/ask_question`,
  `cursor/create_plan`) are answered "method not found"; they could be passed
  to the desktop and phone instead.
- Antigravity CLI: its login is in the keyring, which no session can reach
  (D-Bus is a Unix socket), or else in a file in `~/.gemini/antigravity-cli`
  whose name it doesn't document. "Logged in" therefore means that folder
  exists, and the Linux fix signs in with D-Bus out of reach
  (`DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent agy acp --login`, which
  asks for the code from the browser) so the login goes to the file; **not
  checked end to end**. A session the agent refuses for want of a login now
  fails with that fix. No profiles yet, and no API key login. Its hosts are
  read from the binary (Cloud Code, AI Code, Gemini API, Vertex, OAuth,
  `antigravity.google`) until a real turn runs. Sessions and profiles stored
  as `gemini` (Gemini CLI, which it replaced) load as Antigravity.
- Voice: on the desktop the words appear while you talk (about a second
  behind); the model is multilingual and detects the language itself.
  Recordings up to 10 minutes. The model loads on first use (about 1.5 s,
  roughly 1 GB of memory while loaded) and stays loaded.
- Attachments are copied into the session's temp folder, so the agent can
  read them but they are not in the workspace; they are deleted with the
  session's temp folder when it ends.
- Not checked: each provider's terms for third-party ACP clients on
  subscription logins (the spec lists this before milestone 2).
