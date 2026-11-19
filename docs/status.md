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
| Attachments | Host ✅: `upload.begin/append/finish` (512 KB pieces, 25 MB a file, 10 a message), files kept in `<data>/uploads` (owner-only, outside every sandbox), copied into the session's temp folder at turn start and named in the prompt text; images go inline when the agent takes images, small text files embedded when it takes embedded context, otherwise `resource_link`. Tested with the fake agent over the socket and through the relay (FFI), and with **Cursor** in the sandbox (it read the attached file). Desktop: file picker, drop (not on Wayland: winit has no file drags there), chips, names in the prompt bubble; rendered headless. iOS: written, **not compiled** |
| Live updates and reconnects | Host streams reply chunks live over the socket and through the deployed relay (timed with Cursor and Claude Code: chunks every 20–40 ms). Devices ping the relay every 15 s and treat 40 s of silence as a dead link (tested with a relay that goes silent; an idle link to the deployed relay stays up); the host does the same towards the relay (20 s / 50 s). Both apps re-subscribe a session when the host reports newer events than they hold and they don't arrive within 1.5 s (desktop: tested by dropping the stream; fails without it). iOS reconnects at once when the network switches and every few seconds while the host is offline. iOS changes **not compiled** |
| Quick tasks (no project folder) | Host ✅: built-in `ws_quick` workspace (kind `scratch`), a fresh owner-only folder per session next to the data folder (hidden from every other session), several at once; can't be removed. New `web` network preset (any public host; private and local addresses still refused by the proxy, every connection logged), on for Quick tasks by default, a switch on every workspace. Tested with the fake agent and unit tests; **no real agent has looked anything up through it yet**, and quick tasks' sandbox (a writable folder inside a hidden one, like each session's temp folder) is as untested on macOS as the rest. Desktop ✅ (Workspaces: "Open web" switch, Quick tasks built in). iOS: listed first on New session; builds, not run |
| Projects from the phone | Host ✅: `project.browse` / `project.create` (phones may call them) only inside the project folders, default `~/Code`, set with `project.set_roots` (admin only); hidden folders and symlinks out of a project folder are skipped; the usual workspace checks apply. Tested as a phone connection. CLI ✅ `agenthost project …`. Desktop ✅ "Project folders" card. iOS: "New Project…" on New session (browse, use or create a folder); builds for the simulator, not run |
| Session statuses, archive, per-workspace default login | Host ✅: `session.archive` (ends a live session, hides it from `session.list`; `archived: true` lists them) and `session.unarchive`; workspaces remember a default login, set by their first session and changed with `workspace.set_default_agent` (phones may call all three). Tested with the fake agent, including archive surviving a restart. CLI ✅ (`session archive/unarchive`, `session list --archived`, `workspace default`). Desktop and iOS: statuses (working, needs you, failed, your turn, ended), archive actions, an Archived list in Settings, and the default login preselected in New Session |
| Agent profiles (several logins per agent) | Host ✅: `agent.profile_add/remove/list`, one `host.info` entry per login, `session.create` takes `profile_id`; the folder goes in the agent's own setting (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `GEMINI_CLI_HOME`, OpenCode `XDG_DATA_HOME`, Cursor `XDG_CONFIG_HOME` on Linux; read from each CLI's code). Folders are checked against home, secrets, host data, other logins and workspaces; each session's sandbox hides every other login. Tested with the fake agent (state lands in the profile folder, resume of a removed profile fails) and the policy unit tests; **not tried with a real second subscription**. CLI ✅ (`agenthost agent …`, `claude:work`). Desktop ✅ choose on New session, add/remove on Host status (rendered headless). iOS: choose on New session; builds for the simulator, not run |
| Installing tools | Host ✅: the gateway's built-in `agenthost__install_tool` (npm, pipx, uv, cargo, brew, go; offered only for managers found on PATH) always asks first, shows the exact command, the agent's reason and "runs as you, outside the sandbox", and only then runs it, as an argument list with no shell, one at a time, with a 15-minute limit; registry package names and plain versions only (no URLs, paths, git specs, flags or ranges); decisions and results go to the audit log. Each session can also write a shared session tools folder next to the data folder (npm prefix, pip user base, pipx, uv, cargo and go homes, their `bin` first on PATH), so agents install helpers for themselves without approval and without touching your home folder. Tested with a fake package manager (denied runs nothing, approved runs exactly what was shown) and unit tests; **no real package manager or real agent has used it yet**. Desktop ✅ install card (rendered headless in both looks). iOS: install card and approval sheet; builds, checked in the simulator demo |
| Model and effort | Host ✅: the agent's ACP `configOptions` (select kind; `mode` left out, the host sets it) in `Session.config_options`, changed with `session.set_config`; the last choice per login is applied to new sessions. Tested with the fake agent; **not tried with the real Claude adapter** (shapes read from its source). Desktop and iOS: model and effort pickers in the composer; rendered with demo data |
| Side questions (`/btw`) | Host ✅: `session.side_ask` forks the conversation (ACP `session/fork`, offered when the agent advertises it) and answers alongside a running turn; the fork's output never reaches the main transcript (`side.*` events); forks get no connectors. Tested with the fake agent; **not tried with a real agent**. Desktop: side panel with a tab per question; iOS: a sheet. Rendered with demo data |
| Connector catalog and icons | Host ✅: `connector.search` queries integrations.sh (MCP servers only, icons as `data:` URLs); each connector gets its site's favicon (Google's favicon service, so the domain is sent there) when added. Rename refuses a tool prefix that clashes. Tested against a local server; **the real services were not called by the tests**. Desktop: search when adding, rename, icons (bearer tokens no longer offered). iOS: icons on approvals |
| Pull requests | Host ✅: `agenthost__create_pull_request` (offered when `gh` is on the host's PATH) shows the title, description, base and head, and after approval runs `gh pr create` as you outside the sandbox (description on stdin, no prompts; git commands gh runs get hooks and fsmonitor off through git's environment config). Tested with a fake `gh`; **not tried against GitHub**. Desktop and iOS show the description on the approval |
| Pushing and fetching | Host ✅: the gateway's built-in `agenthost__git_push` asks first (exact command, the agent's reason, a separate "Force-Push" label for `--force-with-lease`), `agenthost__git_fetch` runs without asking; both run git outside the sandbox with your credentials, in the session's worktree or workspace, only against remotes the repository has and branch names (no refspecs, deletes or URLs), with hooks and fsmonitor off and a 5-minute limit; logged to the audit log. Tested with a local bare remote (denied pushes nothing, approved pushes the branch, a planted `pre-push` hook doesn't run); **not tried against GitHub with real credentials**. Desktop and iOS: the built-in approval card takes its button label and warning from the host |
| Screen sharing (POC) | Host ✅ with a fake engine: `screen.status/start/stop/attach/set_policy` (protocol 3). Off until the desktop allows it (`screen.set_policy`, admin only), watching and control allowed separately; `screen.start` starts [screen-relay](https://github.com/vercel-labs/screen-relay) on loopback (30 fps, 0.5–8 Mbps) and returns a one-time ticket; the phone redeems it as the first message of a second tunnel, which then carries screen-relay's `nsl1` messages untouched (ACKs always, input only with control; one viewer at a time, a new one replaces the old; a viewer is ended after 15 minutes; stops 60 s after the last viewer). Modes: your desktop (macOS main display; X11; Wayland compositors with the wlroots protocols, not GNOME or KDE) and, on Linux, a browser in a separate virtual desktop. Tested through the dev relay against a fake engine (`tests/screen.rs`: policy, view-only drops input, control reaches the engine, tickets work once, takeover, idle stop). Against a real engine ✅: screen-relay 0.1.2 in Docker (X11 virtual desktop with Chromium), host on macOS, through the dev relay, `agenthost --remote … screen record --click … --type …`: 37 H.264 frames at 1280×800 arrived and decoded, and the click and the typed text reached the page (19 Nov 2026). **Not run on a Mac's own display, on Hyprland, with a locked screen, or from a real iPhone.** `agenthostd install-tools` installs screen-relay 0.1.2 next to the adapters and builds its engine. CLI ✅ `agenthost screen status|allow|off|stop|record`. Desktop: Settings → Devices → Screen sharing (rendered headless in both looks). iOS: Host Screen (Settings, and the sessions toolbar when allowed): mode picker, landscape while viewing, `AVSampleBufferDisplayLayer` H.264 viewer with tap/hold/drag/pinch and a key bar; decodes a real H.264 clip in the simulator demo; never run against a host |
| Voice input | Host ✅: NVIDIA Nemotron 3.5 ASR streaming 0.6B (int8, sherpa-onnx export, 475 MB) downloaded on request with a pinned SHA-256, run on the CPU through `sherpa-onnx` 2.0.1 (statically linked ONNX Runtime, about 36 MB in `agenthostd`). Download, checksum, unpacking and transcription through `voice.*` checked here (sample French and German clips: about 0.5–0.8 s for 3–5 s of audio after a 1.5 s first load). Desktop: microphone through `cpal` checked (records a WAV); the full speak → text path not tried with a voice. iOS: uses iOS dictation (Speech framework, on device where supported) instead of the host model, so nothing is downloaded for the phone; builds for the simulator, not tried with a voice |

## Milestone 0 answers (Linux)

| Question | Answer |
| --- | --- |
| Claude Code completes a turn in the sandbox | ✅ yes, with shell tool calls and an MCP call through the gateway |
| OpenCode completes a turn in the sandbox | ✅ yes, with shell tool calls and an MCP call through the gateway |
| Full ACP session (prompt, tool call, resume) | ✅ for Claude Code (prompt, tool call, cancel, resume after host restart with history intact); Codex and OpenCode: prompt, tool call, MCP; resume ❌ not tried |
| Agents' built-in sandboxes nested in ours | ❌ not tested. Codex's is switched off (ACP mode `agent-full-access`); Claude Code's and OpenCode's are not used. The host's own sandbox is the boundary |
| Hosts needed for a turn | Codex: `chatgpt.com` ✅. Claude Code: `api.anthropic.com` sufficed (also allowed: `console.anthropic.com`, `claude.ai`, `platform.claude.com`). OpenCode: `opencode.ai`/`models.dev` sufficed for this account's model; it also tries `auth.openai.com` (refused) for an OpenAI login it holds |
| Cursor completes a turn in the sandbox | ✅ yes (CLI 2026.11.13), with an MCP call through the gateway; `~/.cursor` and `~/.ssh` were unreadable from inside. Resume ❌ not tried |
| Gemini CLI completes a turn in the sandbox | ❌ not tried: no Google login on the test machine. Installs (0.68.0), reports "not logged in" with the fix, ACP `initialize` checked |
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

## Deployed relay check (16 Nov 2026)

Against the Worker deployed on Cloudflare, with a scratch host and the fake
agent: the host connects; the CLI pairs as a device (codes matched on both
sides); `status` and a sandboxed session run through the tunnel in about a
second; the device is refused `device.revoke` (capability error); a revoke
from the host drops its open connection in 0.2 s and every later connection
is refused. The host listened on no TCP port throughout.

## Decisions made while building (worth reviewing)

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
  - Gemini CLI: a host-written system settings file (`GEMINI_CLI_SYSTEM_SETTINGS_PATH`,
    which overrides `~/.gemini/settings.json`) allows only the `agenthost` MCP
    server and turns off hooks, extensions, skills, auto-update and usage
    statistics; also `--allowed-mcp-server-names agenthost`. ACP
    `authenticate` with `oauth-personal`.
- **Gateway without the proxy.** Cursor's MCP client ignores `HTTP_PROXY`, so
  Cursor and Gemini get the gateway at the proxy's own address
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
- Gemini CLI: only the Google login is detected (`~/.gemini/oauth_creds.json`);
  an API key login is not offered. Its network hosts are a best guess
  (`generativelanguage.googleapis.com`, `cloudcode-pa.googleapis.com`,
  `oauth2.googleapis.com`, `www.googleapis.com`) until a real turn runs.
- Voice: transcription runs after you stop recording, not live while you
  talk; the model is multilingual and detects the language itself.
  Recordings up to 10 minutes. The model loads on first use (about 1.5 s,
  roughly 1 GB of memory while loaded) and stays loaded.
- Attachments are copied into the session's temp folder, so the agent can
  read them but they are not in the workspace; they are deleted with the
  session's temp folder when it ends.
- Not checked: each provider's terms for third-party ACP clients on
  subscription logins (the spec lists this before milestone 2).
