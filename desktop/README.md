# AgentOS desktop

The full control surface, built with [Rune](https://github.com/anomalyco/rune)
(pinned to rev `e7f3ad9`). It runs on the same machine as the host and talks
to it over the Unix socket; if no host is running it starts `agenthostd run`.
It can also pair with a host on another machine and reach it through the
relay (see [Remote access](#remote-access) below).

Design v2 (nano.design `dohse26rb`): one codebase, two looks. macOS gets AppKit
patterns, Linux gets GNOME / libadwaita. The look follows the OS;
`AGENTHOST_DESKTOP_LOOK=macos|gnome` picks one, so both render on one machine.
`AGENTHOST_DESKTOP_FONT="Liberation Sans"` sets the interface font to any
installed family (the look's own font fills in what it lacks).

- **Sidebar** (compact, nano page "Desktop sidebar v3"): New Session and
  Automations at the top, then Pinned and the workspaces (Quick tasks first),
  each a collapsible group with a "+" for a new session there; the newest 8
  show, "Show N more" the rest. A row is one line: status, title, a pull
  request icon (green open, purple merged, red closed; the host checks open
  ones every 5 minutes with your `gh`) or automation icon and its age; a second line only when it needs
  you, failed or has agents running. A session whose agent finished a turn you
  haven't looked at has a green dot until you open it (remembered between
  launches). Hover a row for Pin and Archive (pins
  need a host on protocol 8). Search sits behind the header's magnifier
  (⌘F / Ctrl+F; Escape clears, then closes). Live sessions are numbered
  ⌘1…⌘9 (Alt+1…9), shown while you hold the key. Collapsed groups are
  remembered in `desktop.json` (`~/Library/Application Support/AgentHost/`,
  or `~/.config/agenthost/`). The host line at the bottom opens status.
- **Session**: the transcript in a 780 pt (GNOME 800 px) column: prompts,
  Markdown replies (streaming), thoughts, tool calls (expand for output),
  blocked hosts with "Allow for <workspace>", approvals inline with
  Deny ⌥⌘D / Approve ⌥⌘A (Alt+D / Alt+A). The toolbar has a status capsule
  (macOS) or the window an approval banner (GNOME), and a session menu.
  Opening a session puts the keyboard in its composer. While the agent works
  the composer's send button stops the turn (⌘. / Ctrl+.). The composer
  attaches files (an image shows a small picture of itself, so pasted ones
  can be told apart), dictates, and shows its keys; ↑ in an
  empty composer brings back your last message. Each session keeps what you
  typed in it while you look at another. Images you sent show as
  pictures in the transcript (hosts on protocol 7); click one to see it in
  the window, Esc to close. Dictation (⇧⌘D / Ctrl+Shift+D,
  or the microphone) writes the words into the composer as you talk; ↩ stops
  and sends, Esc throws the dictation away.
- **Right panel** (`panel.rs`): one trailing pane with a tab per thing it
  shows; today the terminal, the browser, the open session's changes and
  files, and its side questions (`/btw`). A
  new feature for the panel is a `PanelTab`, its entry in `panel::tabs` and
  its body in `panel::pane`. A subagent's pane takes its place while open.
- **Terminal** (⌃` / Ctrl+`, the toolbar's terminal button, GNOME's main
  menu): your login shell on the host, outside every sandbox, in the open
  session's folder (its worktree, else its workspace's; your home folder with
  no session open). "+" opens another, each a tab named for its folder or for
  what the shell calls itself; `exit` closes the tab, and the panel with the
  last one. The host owns the shell, so it runs on while the app is closed
  and the screen is drawn again from what the host kept. ⌘C/⌘V or
  Ctrl+Shift+C/V copy and paste, drag selects (double-click a word, triple a
  line), the wheel and Shift+Page Up read back, and the panel's edge drags
  (remembered in `desktop.json`). Every other key is the shell's while it has
  the focus. Only on the host's own computer: a paired connection has no
  terminal button, and the host would refuse it. `term.rs` is the emulator
  (`alacritty_terminal`, drawn with Rune's `grid`), `terminal.rs` the tab.
- **Browser** (⇧⌘B / Ctrl+Shift+B, the toolbar's globe), in `browser.rs`, for
  hosts on protocol 12: the host's Chromium for the open session's workspace
  (else the one picked for a new session, else Quick tasks), the one that
  workspace's agents drive. Sign in to a site here once and every session of
  the workspace is signed in. Its tabs are a strip (a bolt marks an agent's;
  the panel follows the open session's agent to a tab it opens), with back,
  forward, reload and an address bar (⌘L / Ctrl+L). The page is a picture the
  host sends; clicks, the wheel, keys and paste go back to it, ⌘C / Ctrl+C
  copies its selection, ⌘R / F5 reloads. A tab you open takes the panel's
  size; an agent's keeps its own and is scaled to fit. Hosts the workspace's
  network settings refused are named at the bottom, with "Sign Out
  Everywhere…" (asks first) beside them. Only on the host's own computer.
  The bridge decodes frames (`zune-jpeg`) on its own thread straight into a
  `rune::NativeSurface` (`browser::Shown`), so the UI thread never queues them.
- **Changes** (⇧⌘G / Ctrl+Shift+G, the toolbar's button in a session with a
  branch of its own) and **Files** (⇧⌘E / Ctrl+Shift+E), in `files.rs`, for
  hosts on protocol 11. Changes is everything the session's branch changed
  since it left the remote's main branch (fetched by the host first),
  uncommitted work and new files included
  (what a pull request would show), as one scrolling diff, unified or split;
  a file header's document button opens that file. Files is the session's
  whole folder as a tree with a filter; double-click or Return opens a file
  read only, coloured by language, and "Show Changes" goes back to its part
  of the diff. Both load when shown, when the agent's turn ends, and on
  Refresh. The diff, the tree and the colours are rune-kit's (`diff`,
  `Tree`, `syntax`, 80 languages), which the app already linked for
  Markdown: no new dependency. They work on a paired connection too.
- **New session** (⌘N, quick task ⇧⌘N): a sheet (macOS) or dialog (GNOME):
  workspace and login ("Claude Code · Work"); ⌘↩ starts, and you type the
  first message in the session. Picking a workspace picks its default login
  (tagged "Default"; the first session in a workspace sets it). Another login
  offers "Make … the default for …".
- **Automations** (the sidebar's Automations entry, View › Automations, or
  GNOME's main menu): sessions that start on a schedule, from a webhook
  (Sentry, GitHub, any sender) or by hand. The list shows what starts each,
  its next and last run and an on/off switch; beside it the chosen one's
  webhook URL (Copy, New URL…), last delivery, prompt and runs (click a run to
  open its session). New Automation and Edit… open the editor: workspace,
  login, prompt, schedule presets or a cron (checked by the host as you type),
  webhook sender and secret, what runs without asking, and the longest a run
  may take. Sessions that are runs show a small clock in the sidebar.
- **Archive** (⌃⌘A / Ctrl+Alt+A, the Session and "…" menus, row context
  menus): hides a session; a live one asks first, since archiving ends it.
- **Settings** (⌘, / Ctrl+,): General (connection, host, sandbox, voice,
  relay, Re-run checks), Agents (logins and profiles), Workspaces (network
  rules, extra hosts, connectors, project folders), Connectors (add with URL
  and token, sign in, defaults and per-tool policy), Devices (pair with QR and
  code, rename, revoke), Archived (search, Open read-only, Unarchive;
  archived sessions can't be deleted). Workspaces also sets the default login.
  Drawn over the main window rather than in a window of its own: Rune's
  windows each own their state.
- **Keys**: the macOS menu bar (File, Session, View, Settings…) lists every
  shortcut; on GNOME, Ctrl+? opens the shortcuts window and F10 the main menu.

## Remote access

The app pairs with a host on another machine the way the CLI and the iPhone
do: there, `agenthost device pair` shows a payload and a 6-digit code; here,
Settings → General → Connection → "Pair with a Remote Host…" (or the button
on the offline screen) pastes the payload and shows the same code to compare.
The keys land in `remote.json` in the config folder (`$AGENTHOST_REMOTE` picks
the file, the same one `agenthost remote pair --state` writes), and a laptop
with no host of its own connects there at every launch — a tiny `desktop.json`
in the same folder remembers that choice, and "Use this computer" in the same
place undoes it. `scripts/install.sh --desktop-only` installs just the app for
such a laptop (no host, CLI or service); `--uninstall` removes it again.

A paired connection gets sessions and approvals, never policy: workspaces,
connectors, devices, webhook automations and built-in tool policies render
dimmed with a note saying where to change them, and the host would refuse
them anyway. The offline screen says "the host may be asleep or the relay
unreachable" instead of asking after agenthostd, and offers pairing again.

```sh
cd desktop
cargo run --release
# Headless snapshots of every screen with sample data (no host needed):
AGENTHOST_DESKTOP_OFFLINE=1 AGENTHOST_DESKTOP_LOOK=macos cargo run -- --snap s.png --scene session --size 1280x800
#   scenes: sidebar sidebar-search sidebar-collapsed sidebar-hover sidebar-jump
#           session session-compose session-voice-download session-no-sidebar session-menu main-menu
#           home new new-other-login new-login-menu confirm-archive row-menu failed
#           settings-archived archived-open settings-general settings-agents settings-agents-add
#           settings-workspaces settings-connectors settings-connectors-add settings-devices
#           pairing pairing-confirm shortcuts offline offline-home
#           pair-remote pair-remote-code settings-general-remote settings-devices-remote
#           offline-remote offline-remote-home
#           automations automations-webhook automations-empty automation-new automation-new-cron-error
#           automation-new-webhook automation-edit automation-edit-webhook automation-picker
#           automation-confirm-delete automation-confirm-rotate
#           terminal terminal-home terminal-side terminal-ended terminal-remote
#           changes changes-split files file-open
cargo test      # drives a real in-process host through the app's model and bridge
```

It is a separate Cargo workspace because Rune is a large GPU stack and patches
several crates.io dependencies (winit, wgpu-hal, parley_engine, accesskit),
which the host and CLI should not carry.

Verified headless in both looks on macOS, and the model and bridge against a
real host (sessions, the new-session sheet, approvals through a test MCP
server, pairing through the dev relay).
