# Security model

The sandbox is the boundary. Agents run with their own approvals on auto, so
what keeps them in check is what the sandbox lets them reach, not a prompt
asking "may I?" before every command.

## The sandbox

Every session runs in a sandbox (bubblewrap + seccomp on Linux, Seatbelt on
macOS, via Anthropic's `sandbox-runtime` at a pinned version). It:

- can write only its workspace, its temp folder, its agent's own state and a
  shared session tools folder (where agents install helpers for themselves
  with npm, pipx, uv, cargo or go, never into your home folder);
- cannot read secrets folders, other agents' credentials or the host's data;
- has no network except the host's per-session egress proxy, which allows
  only the agent's provider hosts plus the workspace's network profile,
  resolves names itself and refuses private addresses.

Each agent gets a host-controlled configuration (no personal MCP servers,
hooks or plugins). Nothing from the repository, such as its git hooks, runs
outside the sandbox.

## Leaving the sandbox, with your approval

- **MCP connectors** are reached only through the host's gateway, which holds
  the credentials and asks before write tools by default.
- **Installing a tool** on your computer goes only through the gateway's
  built-in `agenthost__install_tool`: you see the exact command and the
  agent's reason, and it runs as you, outside the sandbox, only after you
  approve (a registry package name and plain version only, no shell, one at
  a time, logged to the audit log).
- **Pushing**: the sandbox can't read your git credentials, so
  `agenthost__git_push` asks you to approve the exact `git push` and the host
  runs it with your credentials (fetching runs without asking).
  `agenthost__create_pull_request` opens a pull request with your GitHub CLI
  after you approve its title and description.
- **Any other command that needs your logins** (`gh pr edit`,
  `gh issue create`, a deploy CLI) goes through `agenthost__run_command`: you
  see the exact argument list and its input, and the host runs it as you in
  the session's folder after you approve, never through a shell and never a
  program from a folder a session can write. "Always Allow" lets a prefix
  such as `gh pr edit` run without asking from then on (listed, and undone,
  under Settings → Connectors); shells, interpreters and build tools always
  ask.

## Remote access and the relay

The host listens on no network port: local clients use a Unix socket, and
the host dials out to the relay. Phones and the host talk through the relay
over end-to-end Noise tunnels (`XXpsk3` to pair, `IK` afterwards), so the
relay forwards ciphertext it cannot read. The one exception is webhook
ingress, which the relay seals to the host in memory as it arrives (see
[relay-protocol.md](relay-protocol.md)).

## Push notifications

Push notifications go from the host to the relay that serves the phone's
build of the app, which passes them to Apple's APNs (or, for builds without
one, from the host straight to APNs). Their content is sealed with a key each
phone gives the host (ChaCha20-Poly1305) and opened on the phone, so Apple
and the relay see only a generic alert. The phone's APNs token goes to that
relay, never to the host.

## Limits

See [status.md](status.md) for what has been verified, how, and on which
platforms. To report a vulnerability, see [SECURITY.md](../SECURITY.md).
