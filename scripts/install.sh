#!/usr/bin/env bash
# Build the Swage release binary (with the frontend embedded) and install it
# into a user-local bin directory so `swage` can be invoked from any shell.
#
# Usage:
#   scripts/install.sh                  # install into ~/.local/bin, loopback only
#   scripts/install.sh --prefix /usr/local/bin
#   scripts/install.sh --skip-frontend  # reuse the existing frontend/dist
#   scripts/install.sh --lan            # also bind 0.0.0.0 so other devices on
#                                       # the LAN can reach the gateway. There
#                                       # is no auth — only enable on networks
#                                       # you trust.
#
# After install, start the gateway with `swage gateway start` and open
# http://127.0.0.1:3000 in a browser.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

prefix="${HOME}/.local/bin"
skip_frontend=0
lan_mode=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --prefix)
            shift
            prefix="$1"
            ;;
        --prefix=*)
            prefix="${1#*=}"
            ;;
        --skip-frontend)
            skip_frontend=1
            ;;
        --lan)
            lan_mode=1
            ;;
        -h|--help)
            sed -n '2,15p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            exit 2
            ;;
    esac
    shift
done

# Cap target/ at ~50 GiB by evicting least-recently-used artefacts. Time-based
# sweeps don't help an actively-built workspace (every dep gets touched on every
# build), so we cap by size instead. `--maxsize` takes megabytes.
# Best-effort: skipped silently if cargo-sweep isn't installed (run
# `cargo install cargo-sweep` to enable). Never fails the install.
if command -v cargo-sweep >/dev/null 2>&1; then
    echo "==> Capping target/ at 50 GiB (cargo sweep --maxsize)"
    cargo sweep --maxsize 50000 || true
else
    echo "==> Skipping cargo sweep (install with: cargo install cargo-sweep)"
fi

if [[ "$skip_frontend" -eq 0 ]]; then
    echo "==> Building frontend (vite → frontend/dist)"
    npm --prefix frontend ci
    npm --prefix frontend run build
else
    if [[ ! -d frontend/dist ]]; then
        echo "frontend/dist is missing — re-run without --skip-frontend" >&2
        exit 1
    fi
    echo "==> Reusing existing frontend/dist"
fi

echo "==> Building swage release binary"
cargo build --release --bin swage --package swage-gateway

mkdir -p "$prefix"
install -m 755 target/release/swage "$prefix/swage"
echo "==> Installed swage to $prefix/swage"

# Persist LAN-binding service defaults so subsequent `swage gateway start`
# calls listen on every interface without needing extra flags. The user can
# revert with `swage gateway uninstall` (which only clears these defaults —
# the binary stays installed).
if [[ "$lan_mode" -eq 1 ]]; then
    "$prefix/swage" gateway install --host 0.0.0.0 --port 3000 >/dev/null
    echo "==> Configured gateway to bind 0.0.0.0:3000 by default"
fi

# Warn if the install prefix isn't on PATH so the user knows to fix it.
case ":${PATH}:" in
    *":${prefix}:"*)
        on_path=1
        ;;
    *)
        on_path=0
        ;;
esac

# Best-effort LAN IP for printing the reachable URL — falls back silently if
# `hostname -I` is unavailable or returns nothing.
lan_ip=""
if command -v hostname >/dev/null 2>&1; then
    lan_ip="$(hostname -I 2>/dev/null | awk '{print $1}')"
fi

echo
echo "Next steps:"

step=1
if [[ "$on_path" -eq 0 ]]; then
    cat <<EOF
  $step. $prefix is not on your PATH yet. Add it, e.g.:
       fish: set -U fish_user_paths $prefix \$fish_user_paths
       bash: echo 'export PATH="$prefix:\$PATH"' >> ~/.bashrc
EOF
    step=$((step + 1))
fi

cat <<EOF
  $step. Start the gateway:
       swage gateway start
EOF
step=$((step + 1))

if [[ "$lan_mode" -eq 1 ]]; then
    cat <<EOF
  $step. Open the UI in a browser:
       Local:  http://127.0.0.1:3000
EOF
    if [[ -n "$lan_ip" ]]; then
        echo "       LAN:    http://${lan_ip}:3000"
    else
        echo "       LAN:    http://<this-machine-ip>:3000"
    fi
    cat <<EOF

LAN exposure warning:
  The gateway has no authentication. Anyone on the same network can drive
  the agent and read your conversation history. Only use --lan on networks
  you trust. To revert, run \`swage gateway uninstall\` (clears the LAN
  default; the binary stays installed) or pass \`--host 127.0.0.1\` to
  \`swage gateway start\` to override per-launch.
EOF
else
    cat <<EOF
  $step. Open http://127.0.0.1:3000 in a browser.
EOF
fi

cat <<EOF

Stop the gateway with \`swage gateway stop\`. Status with \`swage gateway status\`.
Provider keys come from the standard env vars (ANTHROPIC_API_KEY,
OPENAI_API_KEY, GEMINI_API_KEY) or from \$XDG_CONFIG_HOME/swage/config.toml.
EOF
