# agenthost desktop

The full control surface, built with [Rune](https://github.com/anomalyco/rune)
(pinned to rev `e7f3ad9`). It runs on the same machine as the host and talks
to it over the Unix socket; if no host is running it starts `agenthostd run`.

Screens: Sessions (sidebar, grouped by workspace), Session (live transcript,
collapsible tool calls, inline denied hosts with one-click allow, approvals),
New session, Workspaces, Connectors, Devices (pairing QR and code
confirmation, rename, revoke), Host status.

```sh
cd desktop
cargo run --release
# Headless snapshots of every screen with sample data (no host needed):
AGENTHOST_DESKTOP_OFFLINE=1 cargo run -- --snap session.png --scene session --size 1180x760
#   scenes: session new workspaces connectors devices pairing pairing-confirm host offline
cargo test      # drives a real in-process host through the app's model and bridge
```

It is a separate Cargo workspace because Rune is a large GPU stack and patches
several crates.io dependencies (winit, wgpu-hal, parley_engine, accesskit),
which the host and CLI should not carry.

Verified on Linux (headless rendering of every screen; the model and bridge
against a real host). Not run as a window on macOS or Windows yet.
