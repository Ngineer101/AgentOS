#!/usr/bin/env bash
# Install (or update) the host daemon, the CLI and the desktop app as release builds.
#
#   scripts/install.sh                     build, install and (re)start everything
#   scripts/install.sh --relay wss://…     also set the relay for phone pairing (remembered)
#   scripts/install.sh --desktop-only      only the desktop app, to pair with a host on another machine
#   scripts/install.sh --restart           restart the host service only (e.g. after scripts/dev.sh)
#   scripts/install.sh --uninstall         remove the service, binaries and app; keeps your data
#
# Run it again after changing the code: it rebuilds, replaces the binaries and restarts
# the host. Restarting the host stops running agent sessions (they resume where they can).
#
# Installs:
#   ~/.local/bin/agenthostd, ~/.local/bin/agenthost
#   macOS: ~/Applications/AgentOS.app, launch agent ~/Library/LaunchAgents/dev.agenthost.host.plist
#   Linux: ~/.local/bin/agenthost-desktop, ~/.local/share/applications/agenthost.desktop (+ icon),
#          systemd user service ~/.config/systemd/user/agenthost.service
# The host starts at login and restarts if it crashes. Log: <data dir>/logs/host.log
# The relay comes from --relay, else AGENTHOST_RELAY_URL, else the last install, else
# the URL scripts/dev.sh remembered (.dev/relay-url).
#
# --desktop-only installs just the app: no host, CLI, adapters or service, and it needs
# only Rust. With no host on this computer the app opens on its offline screen; "Pair with
# a Remote Host…" there pairs it with your host through the relay (`agenthost device pair`
# on the host shows the link and code). Run it again to update the app.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mode=install relay=""
while (( $# )); do
  a="$1"; shift
  case "$a" in
    --relay) relay="${1:?--relay needs a URL}"; shift ;;
    --relay=*) relay="${a#--relay=}" ;;
    --desktop-only) mode=desktop ;;
    --restart) mode=restart ;;
    --uninstall) mode=uninstall ;;
    -h|--help) sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $a (try --help)" >&2; exit 2 ;;
  esac
done

bin_dir="$HOME/.local/bin"
conf_dir="${XDG_CONFIG_HOME:-$HOME/.config}/agenthost"
relay_file="$conf_dir/relay-url"
os="$(uname)"
if [[ "$os" == Darwin ]]; then
  data="$HOME/Library/Application Support/agenthost"
  label=dev.agenthost.host
  plist="$HOME/Library/LaunchAgents/$label.plist"
  domain="gui/$(id -u)"
  app="$HOME/Applications/AgentOS.app"
  # The app's name before the rename to AgentOS; removed on install and uninstall.
  old_app="$HOME/Applications/AgentHost.app"
elif [[ "$os" == Linux ]]; then
  data="${XDG_DATA_HOME:-$HOME/.local/share}/agenthost"
  unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
  unit="$unit_dir/agenthost.service"
  desktop_file="${XDG_DATA_HOME:-$HOME/.local/share}/applications/agenthost.desktop"
  icon_file="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/512x512/apps/agenthost.png"
else
  echo "unsupported OS: $os (macOS and Linux only)" >&2; exit 1
fi

xml() { local s="$1"; s="${s//&/&amp;}"; s="${s//</&lt;}"; s="${s//>/&gt;}"; printf '%s' "$s"; }

# Copy to a new file and rename it into place: never overwrite a running binary in place
# (on macOS that invalidates its code signature and the process is killed).
put() {
  local src="$1" dst="$2" tmp
  mkdir -p "$(dirname "$dst")"
  tmp="$dst.new.$$"
  cp "$src" "$tmp"
  chmod 755 "$tmp"
  mv -f "$tmp" "$dst"
}

# By command line: Linux cuts process names to 15 characters, so -x never matched the
# 17-character agenthost-desktop there.
desktop_pat='(^|/)agenthost-desktop( |$)'
desktop_running() { pgrep -f "$desktop_pat" >/dev/null; }

stop_desktop() {
  desktop_running || return 0
  echo "==> quitting the desktop app"
  pkill -TERM -f "$desktop_pat" || true
  for _ in $(seq 50); do desktop_running || return 0; sleep 0.1; done
  pkill -KILL -f "$desktop_pat" 2>/dev/null || true
}

stop_service() {
  if [[ "$os" == Darwin ]]; then
    launchctl bootout "$domain/$label" 2>/dev/null || true
  else
    # Signal the host alone first, as KillMode=mixed does: units installed before it
    # signal the agents too, and the sessions then show as failed.
    if systemctl --user is-active --quiet agenthost.service 2>/dev/null; then
      systemctl --user kill --kill-whom=main --signal=TERM agenthost.service 2>/dev/null || true
      for _ in $(seq 300); do
        systemctl --user is-active --quiet agenthost.service 2>/dev/null || break
        sleep 0.1
      done
    fi
    systemctl --user stop agenthost.service 2>/dev/null || true
  fi
  # Any host not run by the service (scripts/dev.sh, or one the desktop started) holds the socket.
  if pgrep -x agenthostd >/dev/null; then
    pkill -TERM -x agenthostd || true
    for _ in $(seq 150); do pgrep -x agenthostd >/dev/null || break; sleep 0.1; done
    pkill -KILL -x agenthostd 2>/dev/null || true
  fi
}

start_service() {
  echo "==> starting the host service"
  if [[ "$os" == Darwin ]]; then
    [[ -f "$plist" ]] || { echo "not installed: run scripts/install.sh first" >&2; exit 1; }
    # bootstrap can briefly fail while launchd is still tearing down the old job.
    local ok=0
    for _ in 1 2 3 4 5; do
      if launchctl bootstrap "$domain" "$plist" 2>/dev/null; then ok=1; break; fi
      sleep 1
    done
    (( ok )) || launchctl bootstrap "$domain" "$plist"
  else
    [[ -f "$unit" ]] || { echo "not installed: run scripts/install.sh first" >&2; exit 1; }
    systemctl --user daemon-reload
    systemctl --user enable --now agenthost.service >/dev/null
  fi
  local log="$data/logs/host.log"
  for _ in $(seq 100); do
    if tail -n 50 "$log" 2>/dev/null | grep -q "host listening"; then
      echo "==> host is running (log: $log)"
      return 0
    fi
    sleep 0.1
  done
  echo "the host has not reported that it is listening yet; last log lines:" >&2
  tail -n 20 "$log" >&2 2>/dev/null || true
}

# The desktop app: the macOS bundle, or the Linux binary with its launcher entry and icon.
install_app() {
  if [[ "$os" == Darwin ]]; then
    echo "==> installing $app"
    rm -rf "$old_app"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
    cp "$root/desktop/assets/AppIcon.icns" "$app/Contents/Resources/AppIcon.icns"
    put "$root/desktop/target/release/agenthost-desktop" "$app/Contents/MacOS/agenthost-desktop"
    # The desktop looks for agenthostd next to itself before PATH (apps get a bare PATH).
    # With --desktop-only there is none; the app then offers pairing with a remote host.
    if [[ -x "$bin_dir/agenthostd" ]]; then
      ln -sfn "$bin_dir/agenthostd" "$app/Contents/MacOS/agenthostd"
    else
      rm -f "$app/Contents/MacOS/agenthostd"
    fi
    cat >"$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>AgentOS</string>
  <key>CFBundleDisplayName</key><string>AgentOS</string>
  <key>CFBundleIdentifier</key><string>dev.agenthost.desktop</string>
  <key>CFBundleExecutable</key><string>agenthost-desktop</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>AgentOS records your voice to turn it into text.</string>
</dict>
</plist>
EOF
    # Ad-hoc signature with a stable identifier, so macOS can attach permissions to the app.
    codesign --force --sign - --identifier dev.agenthost.desktop "$app" >/dev/null 2>&1 || true
    # Finder and the Dock cache icons: tell Launch Services the bundle changed.
    touch "$app"
    /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$app" >/dev/null 2>&1 || true
  else
    echo "==> installing desktop app"
    put "$root/desktop/target/release/agenthost-desktop" "$bin_dir/agenthost-desktop"
    mkdir -p "$(dirname "$desktop_file")" "$(dirname "$icon_file")"
    cp "$root/desktop/assets/app-icon.png" "$icon_file"
    cat >"$desktop_file" <<EOF
[Desktop Entry]
Type=Application
Name=AgentOS
Comment=Run coding agents on this computer
Exec=$bin_dir/agenthost-desktop
Icon=agenthost
Terminal=false
Categories=Development;
EOF
  fi
}

# Reopen the app if the install quit it, else say where to find it.
reopen_or_say_done() {
  if (( was_running )); then
    echo "==> reopening the desktop app"
    if [[ "$os" == Darwin ]]; then open "$app"; else nohup "$bin_dir/agenthost-desktop" >/dev/null 2>&1 & fi
  elif [[ "$os" == Darwin ]]; then
    echo "==> done. Open AgentOS from ~/Applications (or: open \"$app\")"
  else
    echo "==> done. Open AgentOS from your app launcher (or run agenthost-desktop)"
  fi
}

if [[ "$mode" == uninstall ]]; then
  stop_desktop
  stop_service
  if [[ "$os" == Darwin ]]; then
    rm -f "$plist"
    rm -rf "$app" "$old_app"
  else
    systemctl --user disable agenthost.service 2>/dev/null || true
    rm -f "$unit" "$desktop_file" "$icon_file" "$bin_dir/agenthost-desktop"
    systemctl --user daemon-reload 2>/dev/null || true
  fi
  rm -f "$bin_dir/agenthostd" "$bin_dir/agenthost" "$relay_file"
  echo "==> uninstalled. Your data is still in: $data"
  echo "    (delete it to remove sessions, workspaces, pairings and installed tools)"
  exit 0
fi

if [[ "$mode" == restart ]]; then
  stop_service
  start_service
  exit 0
fi

if [[ "$mode" == desktop ]]; then
  echo "==> building desktop (release)"
  cargo build --release --manifest-path "$root/desktop/Cargo.toml"
  was_running=0
  desktop_running && was_running=1
  stop_desktop
  install_app
  if [[ ! -x "$bin_dir/agenthostd" ]]; then
    echo "==> to connect: on your host run \`agenthost device pair\` (or Settings → Devices there),"
    echo "    then open AgentOS here, choose \"Pair with a Remote Host…\" and paste the link"
  fi
  reopen_or_say_done
  exit 0
fi

# Relay URL.
if [[ -z "$relay" ]]; then
  if [[ -n "${AGENTHOST_RELAY_URL:-}" ]]; then relay="$AGENTHOST_RELAY_URL"
  elif [[ -f "$relay_file" ]]; then relay="$(head -n1 "$relay_file")"
  elif [[ -f "$root/.dev/relay-url" ]]; then relay="$(head -n1 "$root/.dev/relay-url")"
  fi
fi
[[ "$relay" == https://* ]] && relay="wss://${relay#https://}"
if [[ -n "$relay" ]]; then
  mkdir -p "$conf_dir"; printf '%s\n' "$relay" >"$relay_file"
fi

echo "==> building host and CLI (release)"
cargo build --release --manifest-path "$root/Cargo.toml" -p agenthost-host -p agenthost-cli
echo "==> building desktop (release)"
cargo build --release --manifest-path "$root/desktop/Cargo.toml"

was_running=0
desktop_running && was_running=1
stop_desktop
stop_service

echo "==> installing binaries into $bin_dir"
put "$root/target/release/agenthostd" "$bin_dir/agenthostd"
put "$root/target/release/agenthost" "$bin_dir/agenthost"

# Pinned adapters and sandbox runtime; quick when nothing changed.
echo "==> installing agent adapters and sandbox runtime"
"$bin_dir/agenthostd" install-tools
mkdir -p "$data/logs"

# Services start with a bare PATH; give the host this shell's, so it finds node and agent CLIs.
host_path="$bin_dir:$PATH"
run_args=("$bin_dir/agenthostd" run)
[[ -n "$relay" ]] && run_args+=(--relay-url "$relay")

if [[ "$os" == Darwin ]]; then
  install_app

  echo "==> installing launch agent $plist"
  mkdir -p "$(dirname "$plist")"
  args_xml=""
  for a in "${run_args[@]}"; do args_xml+="    <string>$(xml "$a")</string>"$'\n'; done
  cat >"$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$label</string>
  <key>ProgramArguments</key>
  <array>
$args_xml  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>$(xml "$host_path")</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <!-- Restart after a crash, not after a clean stop (so scripts/dev.sh can take over). -->
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Interactive</string>
  <key>ExitTimeOut</key><integer>30</integer>
  <key>StandardOutPath</key><string>$(xml "$data/logs/host.log")</string>
  <key>StandardErrorPath</key><string>$(xml "$data/logs/host.log")</string>
</dict>
</plist>
EOF
else
  install_app

  echo "==> installing systemd user service $unit"
  mkdir -p "$unit_dir"
  exec_line=""
  for a in "${run_args[@]}"; do exec_line+="\"${a//\"/\\\"}\" "; done
  cat >"$unit" <<EOF
[Unit]
Description=AgentOS host daemon

[Service]
ExecStart=$exec_line
Environment="PATH=$host_path"
Restart=on-failure
RestartSec=2
# Stop signals the host alone: it stops its agents so their sessions resume after.
# (The default signals every process at once, and the agents' exits read as failures.)
KillMode=mixed
TimeoutStopSec=30
StandardOutput=append:$data/logs/host.log
StandardError=append:$data/logs/host.log

[Install]
WantedBy=default.target
EOF
fi

start_service

if [[ -n "$relay" ]]; then
  echo "==> relay: $relay"
else
  echo "==> no relay set: phone pairing is off (run again with --relay wss://your-relay.workers.dev)"
fi
if [[ ":$PATH:" != *":$bin_dir:"* ]]; then
  echo "note: $bin_dir is not on your PATH; add it to use the AgentOS CLI (agenthost)"
fi

reopen_or_say_done
