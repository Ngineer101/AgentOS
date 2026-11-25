# Using AgentOS

The [README](../README.md) covers setup. This page covers the rest. Everything
here works from the desktop app too; the CLI is shown because it's shorter to
write down.

## Workspaces and sessions

```sh
agenthost status                              # sandbox health, agents, login state
agenthost workspace add ~/code/myproject --profile packages
agenthost run ~/code/myproject claude "Summarise this repo"
agenthost chat ~/code/myproject codex         # interactive
agenthost run ~/code/myproject claude "What's wrong here?" --attach crash.log
agenthost voice download                      # local speech model for voice input (once)
```

A workspace's `--profile` decides what its sessions may reach on the network
besides the agent's own provider: any of `packages`, `github` and `web`
(comma-separated), or `agent-only` for nothing else.

Antigravity is a standalone binary, not one of the pinned tools: install it
with `curl -fsSL https://antigravity.google/cli/install.sh | bash` (or set
`AGENTHOST_ANTIGRAVITY_BIN`), then sign in once with the command
`agenthost status` shows. On Linux that is
`DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent agy acp --login`: sessions
can't reach your keyring, so the login has to be kept in Antigravity's own
folder (`~/.gemini/antigravity-cli`). Sessions see that folder and its
conversations, not your plugins, MCP servers, hooks or skills in
`~/.gemini/config`.

## Quick tasks

No project needed? Use the built-in **Quick tasks** workspace: each session
gets a fresh, empty folder of its own, and open web lets the agent look things
up on any public site (local and private addresses stay refused). Switch open
web off on the desktop's Workspaces screen, or with
`agenthost workspace set quick --profile agent-only`.

```sh
agenthost run quick claude "What changed in the latest Rust release?"
```

## New projects from the phone

The phone can turn a folder inside your project folders (default `~/Code`)
into a workspace, or create a new one there. Only the desktop chooses those
folders (`agenthost project roots --set ~/Code,~/Work`);
`agenthost project ls|use|new` does the same from the CLI.

## Several logins for one agent

Several subscriptions of one agent (a work and a personal Claude account,
say)? Give each its own config folder, as you would with `CLAUDE_CONFIG_DIR`,
`CODEX_HOME`, or `XDG_DATA_HOME` for OpenCode (Cursor: Linux only,
`XDG_CONFIG_HOME`; Antigravity: not yet), and add it as a profile. New sessions then pick it,
here as `agent:name`, on the desktop and the phone from the agent list:

```sh
agenthost agent add claude work --config-dir ~/.claude-work
agenthost agent list                          # shows the login command if needed
agenthost run ~/code/myproject claude:work "Summarise this repo"
```

A profile's folder is writable only to sessions that use it and hidden from
every other session, like the other agents' logins.

## Remote access

Deploy `relay/` to your own Cloudflare account (see
[relay/README.md](../relay/README.md)), start the host with
`--relay-url wss://<your-relay>` (or install it with
`scripts/install.sh --relay wss://<your-relay>`), then `agenthost device pair`
shows a QR code.

To try it without Cloudflare: `agenthostd dev-relay` and
`--relay-url ws://127.0.0.1:8787`; `agenthost remote pair '<qr payload>' --state phone.json`
pairs the CLI as a device, and `agenthost --remote phone.json …` runs any
command through the relay. The desktop app pairs too: Settings → General →
Connection → "Pair with a Remote Host…" (or the button on its offline
screen) pastes the same payload, shows the 6-digit code to compare, and from
then on the laptop starts sessions and answers approvals through the relay —
pairing once, reconnecting on its own, keeping its keys in `remote.json`
(`$AGENTHOST_REMOTE` to choose the file, the same one the CLI's `--state`
writes). A paired app gets no policy rights by design: workspaces,
connectors, devices and webhook automations are read-only over the tunnel
and stay managed from the host's own desktop.

## Notifications

The iPhone app can notify you when a session needs your approval (and, if you
ask, when an agent finishes its turn or a session fails). Turn them on while
pairing or in Settings → Notifications; there is nothing to set up on the
host.

Apple accepts notifications for an app only with the APNs key of the team
that signs it, so the relay of whoever publishes the app holds that key and
passes notifications on to Apple
([relay/README.md](../relay/README.md#push-notifications-if-you-publish-the-ios-app)).
If you build the app yourself under another team and run no such relay, the
host sends to Apple itself with your team's key (`.p8`): the phone asks for
it once per host, or run `agenthost push setup --key AuthKey_XXXXXXXXXX.p8 --team-id <TEAM ID>`.

What a notification says is encrypted to the phone; Apple (and the relay)
only see "A session needs you". `agenthost push status` and `agenthost push
test` show and check it.

## Notes

A note is an idea or a todo kept with a workspace until it's ready to start:
something that needs more thought before an agent works on it. Notes are
plain text, and the first line is the title. Add them on the desktop (Notes
in the sidebar, or New Note Here on a workspace), on the phone (the note
button at the top of the session list, or a long press on a workspace), or:

```sh
agenthost note add "Rate-limit pairing attempts
Back off after 5 failed handshakes a minute." -w ~/code/myproject
agenthost note ls                               # open notes, by workspace
agenthost note done <id>                        # or reopen, edit, rm
```

When a note is ready, Start Session opens the New Session sheet in the note's
workspace, and the new session opens with the note's text in its message
box, to edit before you send it. The note lists the sessions started from it,
and archiving one of them marks the note done. Otherwise only you mark a note
done. Notes live on the host, so the desktop, the CLI and every paired phone
see the same ones; a note never runs anything by itself.

## Terminal

The desktop app has a terminal in its right panel, for the commands you'd
otherwise switch to another window for: the toolbar's terminal button, or
Ctrl+` (the key under Escape). It runs your login shell on the host, as you
and outside every sandbox, starting in the open session's folder (its
worktree, or its workspace's folder), or in your home folder when no session
is open. "+" opens another; each has its own tab.

The host owns the shell, not the app: quit the app and a build keeps
running, open it again and the terminal shows what it printed (the last
megabyte). Typing `exit` closes the tab, and the panel with the last one.
Stopping the host ends its terminals.

Copy and paste are ⌘C and ⌘V on macOS and Ctrl+Shift+C and Ctrl+Shift+V on
Linux (Ctrl+C stays the shell's); drag to select, double-click for a word.
The wheel, or Shift+Page Up, reads back through ten thousand lines. Drag the
panel's edge to make it wider.

A terminal is only for the app on the host's own computer. A paired desktop
or phone gets none: the host refuses it. `AGENTHOST_TERMINAL_SHELL` picks a
shell other than your login one.

## Automations

An automation is a saved session that starts by itself: on a schedule
(routine maintenance, upgrading dependencies), when a webhook arrives (a
Sentry alert to investigate), or when you press Run Now. Each run is an
ordinary session, so its transcript is the run's output; the run list shows
when it ran, why, how it ended and the agent's last words. The newest run's
session stays open for follow-up questions until the next run starts. Set
them up on the desktop (Automations in the sidebar), on the phone (Settings →
Automations; schedules and Run Now, not webhooks or pre-approved tools), or:

```sh
agenthost automation add "Upgrade dependencies" -w ~/code/myproject -a claude \
  --cron "0 3 * * 1" --auto-approve create_pull_request,git_push \
  --prompt "Upgrade dependencies, run the tests, open a pull request."
agenthost automation ls                         # next run, last result
agenthost automation run <id> --wait            # now, and print the summary
agenthost automation runs <id>                  # history; each run names its session
```

Schedules are cron in this computer's time (at most every 5 minutes). The
host only runs while the computer is awake: a time missed by up to 12 hours
runs once when it's back, an older one is recorded as skipped. A run still
going after its time limit (60 minutes by default) is stopped, and a
scheduled run is skipped while the previous one is still going. An
automation may let its runs open pull requests and push their own branch
without asking (desktop only; a force-push still asks); anything else asks as
usual, and your phone gets 12 hours to answer instead of 10 minutes.

### Webhooks

Webhooks (desktop only) come in through your relay: each webhook automation
gets a URL `https://<your-relay>/v1/hook/<relay id>/<hook id>`. The relay
seals each request to the host the moment it arrives, keeps it (sealed, for
up to a day) until the host has it, and answers the sender at once; the host
checks the sender's signature, filters and dedupes, then starts a run. This
is the one place the relay sees plaintext: the request while it seals it, in
memory, on your Cloudflare account (see
[relay-protocol.md](relay-protocol.md#webhooks)).

For Sentry:

1. Connect the Sentry MCP connector (Settings → Connectors) and switch it on
   for the workspace, so runs can read the issue.
2. `agenthost automation add "Investigate Sentry alerts" -w ~/code/myproject -a claude --webhook sentry --secret <client secret>`
   (or New Automation → Webhook → Sentry on the desktop) and copy its webhook URL.
3. In Sentry: Settings → Developer Settings → Custom Integrations → New
   Internal Integration. Paste the URL as the Webhook URL, turn on **Alert
   Rule Action**, save, and copy its Client Secret into the automation.
4. In the alert rules you want investigated, add the action "Send a
   notification via" your integration.

Issue alerts, new issues and metric alerts (not resolved ones) start a run;
the same issue starts at most one run a day. Alert text is passed to the agent
as untrusted data, never as instructions. GitHub webhooks (`--webhook github`,
signed with the webhook's secret) and any other sender (`--webhook generic`,
optionally with a bearer token) work the same way; the payload is attached to
the run as `webhook.json` and fields can go in the prompt as
`{{payload.some.field}}`.

## Debugging

```sh
AGENTHOST_ACP_TRACE=1 agenthostd run          # log raw ACP traffic per session
```

The installed host logs to `<data dir>/logs/host.log` (Linux:
`~/.local/share/agenthost`, macOS: `~/Library/Application Support/agenthost`).
