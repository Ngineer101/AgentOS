# agenthost

*(working name)* A self-hosted agent host. A daemon on your own Linux or macOS
machine runs **Codex**, **Claude Code**, **OpenCode**, **Cursor** and
**Gemini CLI** on your existing
subscriptions, each session inside an OS sandbox, with approvals on auto
because the sandbox is the boundary. A desktop app, a command-line client and
an iOS app control it through one protocol; the phone reaches it through a
relay that only ever carries ciphertext. All data stays on the host.

The host never implements its own agent loop: each CLI runs as shipped, driven
over the [Agent Client Protocol](https://agentclientprotocol.com) (ACP).

## Layout

| Path | What |
| --- | --- |
| `crates/proto` | Shared protocol: JSON-RPC types, method/capability table, and the Noise tunnel (pairing, handshakes, framing). One implementation for host, CLI and (via bindings) iOS |
| `crates/host` | `agenthostd`: session manager, ACP client, sandbox launcher, egress proxy, MCP gateway, SQLite event log, relay connector, dev relay |
| `crates/cli` | `agenthost`: the command-line client (everything the desktop can do), also usable as a paired remote device |
| `crates/fake-agent` | A scripted ACP agent for tests |
| `desktop/` | The desktop app (Rune), its own Cargo workspace |
| `relay/` | The relay: a Cloudflare Worker with one Durable Object per host |
| `docs/` | [Relay protocol](docs/relay-protocol.md), [status against the v1 spec](docs/status.md) |

## Quick start (Linux)

```sh
# Prerequisites: Rust, Node 22+, bubblewrap, socat, ripgrep
cargo build --release
./target/release/agenthostd install-tools     # pinned adapters + sandbox runtime
./target/release/agenthostd run &             # listens only on a Unix socket

agenthost status                              # sandbox health, agents, login state
agenthost workspace add ~/code/myproject --profile packages
agenthost run ~/code/myproject claude "Summarise this repo"
agenthost chat ~/code/myproject codex         # interactive
agenthost run ~/code/myproject claude "What's wrong here?" --attach crash.log
agenthost voice download                      # local speech model for voice input (once)
```

No project needed? Use the built-in **Quick tasks** workspace: each session
gets a fresh, empty folder of its own, and open web lets the agent look things
up on any public site (local and private addresses stay refused). Switch open
web off on the desktop's Workspaces screen, or with
`agenthost workspace set quick --profile agent-only`.

```sh
agenthost run quick claude "What changed in the latest Rust release?"
```

New projects from the phone: the phone can turn a folder inside your project
folders (default `~/Code`) into a workspace, or create a new one there. Only
the desktop chooses those folders (`agenthost project roots --set ~/Code,~/Work`);
`agenthost project ls|use|new` does the same from the CLI.

Several subscriptions of one agent (a work and a personal Claude account,
say)? Give each its own config folder, as you would with `CLAUDE_CONFIG_DIR`,
`CODEX_HOME`, `GEMINI_CLI_HOME`, or `XDG_DATA_HOME` for OpenCode (Cursor: Linux
only, `XDG_CONFIG_HOME`), and add it as a profile. New sessions then pick it,
here as `agent:name`, on the desktop and the phone from the agent list:

```sh
agenthost agent add claude work --config-dir ~/.claude-work
agenthost agent list                          # shows the login command if needed
agenthost run ~/code/myproject claude:work "Summarise this repo"
```

A profile's folder is writable only to sessions that use it and hidden from
every other session, like the other agents' logins.

To install the host as a login service plus the CLI and desktop app (macOS or
Linux), run `scripts/install.sh --relay wss://<your-relay>`; run it again after
pulling or changing code to rebuild and restart everything. `--uninstall` removes
it and keeps your data. For a throwaway test run instead, use `scripts/dev.sh`.

Remote access: deploy `relay/` to your own Cloudflare account (see
[relay/README.md](relay/README.md)), start the host with
`--relay-url wss://<your-relay>`, then `agenthost device pair` shows a QR
code. To try it without Cloudflare: `agenthostd dev-relay` and
`--relay-url ws://127.0.0.1:8787`; `agenthost remote pair '<qr payload>' --state phone.json`
pairs the CLI as a device, and `agenthost --remote phone.json …` runs any
command through the relay.

## Security model in one paragraph

Every session runs in a sandbox (bubblewrap + seccomp on Linux, Seatbelt on
macOS, via Anthropic's `sandbox-runtime` at a pinned version) that can write
only its workspace, its temp folder, its agent's own state and a shared
session tools folder (where agents install helpers for themselves with npm,
pipx, uv, cargo or go, never into your home folder), cannot read
secrets folders, other agents' credentials or the host's data, and has no
network except the host's per-session egress proxy, which allows only the
agent's provider hosts plus the workspace's profile, resolves names itself
and refuses private addresses. Each agent gets a host-controlled
configuration (no personal MCP servers, hooks or plugins). MCP connectors are
reached only through the host's gateway, which holds the credentials and asks
before write tools by default. Installing a tool on your computer goes only
through the gateway's built-in `agenthost__install_tool`: you see the exact
command and the agent's reason, and it runs as you, outside the sandbox, only
after you approve (a registry package name and plain version only, no shell,
one at a time, logged to the audit log). Pushing works the same way: the
sandbox can't read your git credentials, so `agenthost__git_push` asks you
to approve the exact `git push` and the host runs it with your credentials
(fetching runs without asking), and `agenthost__create_pull_request` opens
a pull request with your GitHub CLI after you approve its title and
description; nothing from the repository, such as its hooks, runs outside
the sandbox. The host listens on no network port. See
[docs/status.md](docs/status.md) for what is verified and the limits.

## Development

```sh
scripts/dev.sh                                           # build + run host and desktop together (--fresh, --fake)
cargo test --workspace                                   # unit + integration tests
AGENTHOST_TEST_TOOLS=<data-dir>/tools cargo test -p agenthost-host --test m3_escape   # sandbox escape suite
cd relay && npm test                                     # relay tests
AGENTHOST_ACP_TRACE=1 agenthostd run                     # log raw ACP traffic per session
```

## Licence

MIT OR Apache-2.0, at your option.
