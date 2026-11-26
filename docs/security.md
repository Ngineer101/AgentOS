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
  your home folder is otherwise readable, so "secrets" has to mean more than
  the obvious credential folders: `~/.ssh` and friends, but also browser
  profiles (a cookie store is a live login), chat apps' tokens, shell and
  tool history, and database and cloud credential files. The list is
  `SECRET_DIRS` in `crates/host/src/sandbox.rs`. It is a list of what an
  agent may **not** read rather than a list of what it may, because an agent
  needs to see your tools' configuration to work in your repositories — so
  if you keep credentials somewhere unusual, add it there;
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
  runs it with your credentials. The approval also shows where the remote
  really points, because a repository's own settings can send an approved
  "push to origin" somewhere else. `agenthost__create_pull_request` opens a
  pull request with your GitHub CLI after you approve its title and
  description.
- **Fetching** changes nothing on the remote, so it runs without asking —
  but only in the worktree the host made for the session, whose git settings
  the sandbox cannot write. Anywhere else (a folder the agent ran `git init`
  in, a repository inside the session's folder, a shared folder, a quick
  task) the settings are the agent's, and a fetch there asks. Git is run so
  that nothing in those settings can name a command for it to run, whichever
  folder it is: no hooks, no `sshCommand`, no credential helper, no proxy
  command, no transport helper, no submodules.
- **Any other command that needs your logins** (`gh pr edit`,
  `gh issue create`, a deploy CLI) goes through `agenthost__run_command`: you
  see the exact argument list and its input, and the host runs it as you in
  the session's folder after you approve, never through a shell and never a
  program from a folder a session can write. "Always Allow" lets a prefix
  such as `gh pr edit` run without asking from then on (listed, and undone,
  under Settings → Connectors); shells, interpreters and build tools always
  ask.

## Your own terminal

The desktop app's terminal is the one deliberate hole in all of the above,
and it is yours, not an agent's: a login shell the host runs as you, outside
every sandbox, with your logins, exactly as a terminal window would be.

- Only the local socket may use it. Every `terminal.*` method needs the admin
  capability, which paired devices never hold, so a phone or a paired desktop
  can neither open a terminal nor read or type into one of yours, and hears
  nothing about them.
- Agents can't reach it: a session can't connect to the host's socket, and
  the gateway offers no terminal tool.
- Opening and closing a terminal is written to the audit log (the folder, the
  shell, its process id). What you type and what it prints are not logged,
  and the output the host keeps for replay lives in memory only.

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
