# AgentOS

A self-hosted agent host. A daemon on your own Linux or macOS machine runs
**Claude Code**, **Codex**, **OpenCode**, **Cursor** and Google's
**Antigravity CLI** (`agy`) on your existing subscriptions, each session
inside an OS sandbox. You drive it from a desktop app, a command-line client
or an iOS app; the phone reaches your machine through a relay you deploy
yourself, which only ever carries ciphertext. Your code, transcripts and
credentials stay on your machine.

> **Status:** early, and built for one person's own use. It's developed
> mainly on Linux; macOS and the iOS app have had less testing.
> [docs/status.md](docs/status.md) records what has been verified and how.
> Expect rough edges.

The commands (`agenthost`, `agenthostd`), settings (`AGENTHOST_*`) and folders
keep the project's earlier name, agenthost.

## How it works

```
 ┌───────────── your computer ─────────────┐
 │                                         │
 │  desktop app ─┐                         │
 │               ├─ Unix socket ─┐         │        ┌──────────────────┐
 │  agenthost ───┘  (CLI)        │         │        │  relay           │      ┌────────┐
 │                               ▼         │  wss   │  (Cloudflare     │ wss  │ iPhone │
 │                          agenthostd ────┼───────►│  Worker, yours)  │◄─────┤  app   │
 │                               │         │        └──────────────────┘      └────────┘
 │             ┌─────────────────┼──────┐  │         forwards end-to-end
 │             ▼                 ▼      ▼  │         encrypted frames only
 │         sandbox           sandbox  ...  │
 │      claude-agent-acp     codex-acp     │
 │             │                 │         │
 │             └── egress proxy ─┘         │
 │                 (allow-list)            │
 └─────────────────────────────────────────┘
```

- **The host never runs its own agent loop.** Each CLI runs as shipped,
  driven over the [Agent Client Protocol](https://agentclientprotocol.com)
  (ACP) through its pinned adapter. Your existing logins and subscriptions
  are used as they are.
- **The sandbox is the boundary.** Each session runs in bubblewrap + seccomp
  (Linux) or Seatbelt (macOS) via Anthropic's `sandbox-runtime`. It can write
  only its workspace and its own state, can't read your secrets or other
  agents' logins, and reaches the network only through a per-session proxy
  that allows the agent's provider plus the hosts you allow. That's why
  agent approvals can be left on auto.
- **Anything that needs your credentials asks you first.** MCP connectors,
  `git push`, opening pull requests, installing tools and other commands that
  need your logins go through the host, which shows you exactly what will run
  and waits for approval (on the desktop or your phone).
- **The host listens on no network port.** Local clients use a Unix socket.
  For remote access the host dials out to the relay; phones pair by QR code
  and talk to the host over end-to-end Noise tunnels the relay can't read.
- **Everything is logged on the host.** Sessions are an append-only event log
  in SQLite, so clients can disconnect and catch up without gaps.

More detail: [security model](docs/security.md),
[relay protocol](docs/relay-protocol.md).

## Setup

### 1. The host (Linux or macOS)

Prerequisites: [Rust](https://rustup.rs) 1.85+, Node 22+, and the agent CLIs
you want to use, installed and logged in. On Linux also `bubblewrap`,
`socat` and `ripgrep` (on Ubuntu 24.04, unprivileged user namespaces must be
allowed for bubblewrap: `sudo sysctl kernel.apparmor_restrict_unprivileged_userns=0`).

```sh
git clone https://github.com/Ngineer101/relay.git agentos && cd agentos
scripts/install.sh            # build and install the host, the CLI and the desktop app
agenthost status              # sandbox health, agents, login state
```

`scripts/install.sh` puts `agenthostd` and `agenthost` in `~/.local/bin`,
downloads the pinned ACP adapters and sandbox runtime (`agenthostd
install-tools`), and starts the host at login (systemd user service on Linux,
launch agent on macOS). Run it again after pulling to rebuild and restart; `--uninstall`
removes it and keeps your data. To try things without installing, use
`scripts/dev.sh` (builds and runs the host and desktop together; `--fresh`
for a throwaway data folder).

### 2. Your first session

```sh
agenthost workspace add ~/code/myproject --profile packages
agenthost run ~/code/myproject claude "Summarise this repo"
agenthost chat ~/code/myproject codex         # interactive
```

Or open the desktop app (`agenthost-desktop`, or AgentOS in your
Applications on macOS) and press ⌘N / Ctrl+N.

### 3. Remote access (optional)

Deploy the relay to your own Cloudflare account (the Workers Free plan is
enough):

```sh
cd relay && npm ci && npx wrangler login && npx wrangler deploy
```

Then point the host at it and pair a device:

```sh
scripts/install.sh --relay wss://agenthost-relay.<your-subdomain>.workers.dev
agenthost device pair         # shows a QR code (or use Settings → Devices on the desktop)
```

See [relay/README.md](relay/README.md) for what the relay stores and logs. No
Cloudflare account? `agenthostd dev-relay` runs one locally for testing.

### 4. The iOS app (optional)

There is no App Store build; you build it yourself on a Mac with Xcode,
XcodeGen and rustup. See [ios/README.md](ios/README.md), including the
optional push-notification setup (needs a paid Apple Developer account).

## Documentation

| | |
| --- | --- |
| [docs/usage.md](docs/usage.md) | Quick tasks, projects, several logins per agent, notifications, automations and webhooks |
| [docs/security.md](docs/security.md) | The security model in full |
| [docs/relay-protocol.md](docs/relay-protocol.md) | The relay wire protocol, pairing and tunnels |
| [docs/status.md](docs/status.md) | What has been verified, how, and the known limits |
| [desktop/README.md](desktop/README.md) | The desktop app |
| [ios/README.md](ios/README.md) | Building and running the iOS app |
| [relay/README.md](relay/README.md) | Deploying and testing the relay |

## Repository layout

| Path | What |
| --- | --- |
| `crates/proto` | Shared protocol: JSON-RPC types, method/capability table, and the Noise tunnel (pairing, handshakes, framing). One implementation for host, CLI and (via bindings) iOS |
| `crates/host` | `agenthostd`: session manager, ACP client, sandbox launcher, egress proxy, MCP gateway, SQLite event log, relay connector, dev relay |
| `crates/cli` | `agenthost`: the command-line client (everything the desktop can do), also usable as a paired remote device |
| `crates/ffi` | UniFFI bindings to the protocol code for the iOS app |
| `crates/fake-agent` | A scripted ACP agent for tests |
| `desktop/` | The desktop app ([Rune](https://github.com/anomalyco/rune)), its own Cargo workspace |
| `ios/` | The SwiftUI iOS app |
| `relay/` | The relay: a Cloudflare Worker with one Durable Object per host |
| `website/` | The landing page and basic docs: static files served by a Cloudflare Worker |
| `scripts/` | Install and development scripts |

## Development

```sh
scripts/dev.sh                                           # build + run host and desktop together (--fresh, --fake)
cargo test --workspace                                   # unit + integration tests
AGENTHOST_TEST_TOOLS=<data-dir>/tools cargo test -p agenthost-host --test m3_escape   # sandbox escape suite
cd desktop && cargo test                                 # desktop model and bridge
cd relay && npm test                                     # relay tests
AGENTHOST_ACP_TRACE=1 agenthostd run                     # log raw ACP traffic per session
```

## Contributing

This is a personal project, shared so you can run it and adapt it. The best
way to make it your own is to **fork it and change your fork**. If you'd like
to help this project, a **detailed issue** (what you did, what you expected,
what happened, your platform and versions) is more useful than a pull
request. See [CONTRIBUTING.md](CONTRIBUTING.md). Please report security
problems privately, as described in [SECURITY.md](SECURITY.md).

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option. Unless you explicitly state
otherwise, any contribution intentionally submitted for inclusion in this
work by you, as defined in the Apache-2.0 license, shall be dual licensed as
above, without any additional terms or conditions.

AgentOS is an independent project, not affiliated with or endorsed by
Anthropic, OpenAI, Cursor, Google or the makers of OpenCode. Product names
belong to their owners.
