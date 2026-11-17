#!/usr/bin/env bash
# Build and run the host daemon and the desktop app together for local testing.
#
#   scripts/dev.sh            your normal data dir: connectors, workspaces and logins carry over
#   scripts/dev.sh --fresh    a throwaway host in .dev/ (empty database; reuses installed tools)
#   scripts/dev.sh --fake     also offer the scripted fake agent
#   scripts/dev.sh --release  optimised builds
#
# Any running agenthostd is stopped first so the desktop talks to the code you just built.
# Closing the desktop window (or Ctrl-C) stops the host. Host log: <data dir>/logs/dev-host.log
# Set AGENTHOST_RELAY_URL to connect the host to a relay for the phone.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fresh=0 fake=0 profile=debug cargo_flags=()
for a in "$@"; do
  case "$a" in
    --fresh) fresh=1 ;;
    --fake) fake=1 ;;
    --release) profile=release cargo_flags=(--release) ;;
    -h|--help) sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $a (try --help)" >&2; exit 2 ;;
  esac
done

echo "==> building host and CLI"
cargo build "${cargo_flags[@]}" --manifest-path "$root/Cargo.toml" -p agenthost-host -p agenthost-cli
echo "==> building desktop"
cargo build "${cargo_flags[@]}" --manifest-path "$root/desktop/Cargo.toml"

hostd="$root/target/$profile/agenthostd"
desktop="$root/desktop/target/$profile/agenthost-desktop"

if [[ "$(uname)" == Darwin ]]; then
  real_data="$HOME/Library/Application Support/agenthost"
else
  real_data="${XDG_DATA_HOME:-$HOME/.local/share}/agenthost"
fi

host_args=()
if (( fresh )); then
  data="$root/.dev/data"
  # Unix socket paths are limited to ~100 bytes, so keep it short.
  sock_dir="${XDG_RUNTIME_DIR:-/tmp}/agenthost-dev-$(id -u)"
  mkdir -p "$data" "$sock_dir"
  export AGENTHOST_SOCKET="$sock_dir/host.sock"
  host_args+=(--data-dir "$data")
  if [[ ! -e "$data/tools" && -d "$real_data/tools" ]]; then
    ln -s "$real_data/tools" "$data/tools"
  fi
else
  data="$real_data"
fi

if [[ ! -d "$data/tools/node_modules" ]]; then
  echo "==> installing agent adapters and sandbox runtime (first run only)"
  "$hostd" "${host_args[@]}" install-tools
fi

if pgrep -x agenthostd >/dev/null; then
  echo "==> stopping the running agenthostd"
  pkill -TERM -x agenthostd || true
  for _ in $(seq 50); do pgrep -x agenthostd >/dev/null || break; sleep 0.1; done
  pkill -KILL -x agenthostd 2>/dev/null || true
fi

mkdir -p "$data/logs"
log="$data/logs/dev-host.log"
run_args=(run)
(( fake )) && run_args+=(--enable-fake-agent)

echo "==> starting host (log: $log)"
"$hostd" "${host_args[@]}" "${run_args[@]}" >"$log" 2>&1 &
host_pid=$!
cleanup() {
  if kill -0 "$host_pid" 2>/dev/null; then
    echo "==> stopping host"
    kill -TERM "$host_pid" 2>/dev/null || true
    wait "$host_pid" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

# Wait until the host listens so the desktop doesn't start a second daemon of its own.
for _ in $(seq 150); do
  if ! kill -0 "$host_pid" 2>/dev/null; then
    echo "host exited during startup; last log lines:" >&2
    tail -n 20 "$log" >&2
    exit 1
  fi
  grep -q "host listening" "$log" 2>/dev/null && break
  sleep 0.1
done

echo "==> starting desktop (close the window or press Ctrl-C to stop both)"
"$desktop"
