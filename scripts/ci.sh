#!/usr/bin/env bash
# Run the CI checks on this machine: the same tests .github/workflows/ci.yml runs.
#
#   scripts/ci.sh                  only the parts your branch changes (vs origin/main, uncommitted work included)
#   scripts/ci.sh --all            host, relay and desktop
#   scripts/ci.sh host desktop     just these parts (host, relay, desktop)
#   scripts/ci.sh --install-hook   run this before every `git push` (skip once with --no-verify)
#
# The host tests use cargo-nextest when it's installed (cargo install cargo-nextest), else cargo test.
# The sandbox escape suite needs the pinned sandbox runtime; it goes in .dev/ci/tools
# the first time, or set AGENTHOST_TEST_TOOLS to a tools dir of your own.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

parts=() all=0
while (( $# )); do
  case "$1" in
    --all) all=1 ;;
    host|relay|desktop) parts+=("$1") ;;
    --install-hook)
      hooks="$(git rev-parse --path-format=absolute --git-common-dir)/hooks"
      if [[ -n "$(git config core.hooksPath || true)" ]]; then
        echo "core.hooksPath is set ($(git config core.hooksPath)); add scripts/ci.sh to your pre-push hook there" >&2
        exit 1
      fi
      if [[ -e "$hooks/pre-push" ]] && ! grep -q 'scripts/ci.sh' "$hooks/pre-push"; then
        echo "$hooks/pre-push already exists; add scripts/ci.sh to it yourself" >&2
        exit 1
      fi
      mkdir -p "$hooks"
      cat >"$hooks/pre-push" <<'HOOK'
#!/usr/bin/env bash
# Installed by scripts/ci.sh --install-hook. Skip once with `git push --no-verify`.
exec "$(git rev-parse --show-toplevel)/scripts/ci.sh"
HOOK
      chmod +x "$hooks/pre-push"
      echo "pre-push hook installed: $hooks/pre-push (for every worktree of this repo)"
      exit 0 ;;
    -h|--help) sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
  esac
  shift
done

# Which parts to run. These patterns follow the filters in ci.yml.
rust_files='^(crates/|Cargo\.(toml|lock)$|\.config/nextest\.toml$|scripts/ci\.sh$)'
if (( all )); then
  parts=(host relay desktop)
elif (( ${#parts[@]} == 0 )); then
  if base="$(git merge-base HEAD origin/main 2>/dev/null)"; then
    changed="$( { git diff --name-only "$base"; git ls-files --others --exclude-standard; } | sort -u)"
    grep -qE "$rust_files" <<<"$changed" && parts+=(host)
    grep -qE '^(relay/|scripts/ci\.sh$)' <<<"$changed" && parts+=(relay)
    grep -qE "$rust_files|^desktop/" <<<"$changed" && parts+=(desktop)
    if (( ${#parts[@]} == 0 )); then
      echo "==> nothing to test: no host, relay or desktop changes since origin/main (--all runs everything)"
      exit 0
    fi
  else
    echo "==> no origin/main to compare with; running everything"
    parts=(host relay desktop)
  fi
fi

if cargo nextest --version >/dev/null 2>&1; then
  nextest=1
else
  nextest=0
  echo "==> cargo-nextest not found, using cargo test (cargo install cargo-nextest is faster)"
fi

failed=()
step() { # step <name> <command...>: run it, remember a failure, keep going
  local name="$1"; shift
  echo "==> $name"
  if ! "$@"; then failed+=("$name"); fi
}

sandbox_tools() {
  if [[ -n "${AGENTHOST_TEST_TOOLS:-}" ]]; then return; fi
  local pin tools="$root/.dev/ci/tools"
  pin="$(sed -nE 's/^pub const SANDBOX_RUNTIME_PACKAGE: &str = "(.*)";/\1/p' crates/host/src/agents.rs)"
  if [[ -z "$pin" ]]; then
    echo "can't find SANDBOX_RUNTIME_PACKAGE in crates/host/src/agents.rs" >&2
    return 1
  fi
  if [[ "$(cat "$tools/.pin" 2>/dev/null)" != "$pin" ]]; then
    echo "==> installing $pin into .dev/ci/tools"
    # Called from `if` in step(), where set -e is off, so each failure returns by hand.
    mkdir -p "$tools" || return 1
    [[ -f "$tools/package.json" ]] || echo '{"name":"agenthost-ci-tools","private":true}' >"$tools/package.json"
    npm install --save-exact --no-fund --no-audit --prefix "$tools" "$pin" >/dev/null || return 1
    echo "$pin" >"$tools/.pin"
  fi
  export AGENTHOST_TEST_TOOLS="$tools"
}

run_host() {
  if [[ "$(uname)" == Linux ]]; then
    for bin in bwrap socat rg; do
      command -v "$bin" >/dev/null || echo "warning: $bin is missing; the sandbox tests need it" >&2
    done
  fi
  sandbox_tools || return 1
  if (( nextest )); then
    cargo nextest run --workspace
  else
    cargo test --workspace --no-fail-fast -- --test-threads 4
  fi
}

run_relay() (
  cd relay
  if [[ ! -d node_modules || package-lock.json -nt node_modules ]]; then npm ci || return 1; fi
  npx tsc --noEmit && npm test
)

# Not nextest: the desktop tests that start a host take turns through a lock
# in the test binary, which separate processes don't share.
run_desktop() (
  cd desktop
  cargo test --no-fail-fast
)

started=$SECONDS
for p in "${parts[@]}"; do
  step "$p" "run_$p"
done

echo
if (( ${#failed[@]} )); then
  echo "==> FAILED: ${failed[*]} ($(( SECONDS - started ))s)"
  exit 1
fi
echo "==> passed: ${parts[*]} ($(( SECONDS - started ))s)"
