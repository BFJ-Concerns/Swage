#!/usr/bin/env bash
# Run the gateway and the Vite frontend dev server side-by-side.
#
# This is the fast-iteration loop for UI work:
#   - Vite gives sub-second hot reload on Svelte/CSS changes (no rebuild).
#   - cargo run is incremental, so backend changes recompile only what moved.
#   - Both processes share this terminal; Ctrl-C cleanly stops both.
#
# Usage:
#   scripts/dev.sh                 # gateway 3000, Vite 5173 on all interfaces
#   scripts/dev.sh --gateway-port 3030
#
# Open http://localhost:5173 or the printed LAN URL once both have started.
# Vite proxies /api and the WebSocket through to the gateway port — keep them
# aligned with vite.config.ts. LAN mode has no auth; only run it on networks
# you trust.
#
# Always a debug build — release takes minutes to compile, which is the wrong
# trade-off for iteration. Use scripts/install.sh when you want a release build.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

gateway_port=3000     # matches the vite.config.ts proxy target
vite_host=0.0.0.0
vite_port=5173

while [[ $# -gt 0 ]]; do
    case "$1" in
        --gateway-port)
            shift
            gateway_port="$1"
            ;;
        --gateway-port=*)
            gateway_port="${1#*=}"
            ;;
        -h|--help)
            sed -n '2,16p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            exit 2
            ;;
    esac
    shift
done

# Warn if the gateway port doesn't match what vite expects, since the proxy
# target is hardcoded in vite.config.ts.
expected_proxy_port="$(grep -Eo 'http://127\.0\.0\.1:[0-9]+' frontend/vite.config.ts | head -n 1 | grep -Eo '[0-9]+$' || true)"
if [[ -n "$expected_proxy_port" && "$gateway_port" != "$expected_proxy_port" ]]; then
    echo "==> Note: vite.config.ts proxies /api to port ${expected_proxy_port}, but" >&2
    echo "    the gateway will run on ${gateway_port}. Update either to match." >&2
fi

# Track child PIDs so the trap can stop them in order.
gateway_pid=""
vite_pid=""

cleanup() {
    # Disable the trap to avoid recursing if a kill itself fails.
    trap - EXIT INT TERM
    echo
    echo "==> Stopping dev processes"
    if [[ -n "$vite_pid" ]] && kill -0 "$vite_pid" 2>/dev/null; then
        kill "$vite_pid" 2>/dev/null || true
        wait "$vite_pid" 2>/dev/null || true
    fi
    if [[ -n "$gateway_pid" ]] && kill -0 "$gateway_pid" 2>/dev/null; then
        kill "$gateway_pid" 2>/dev/null || true
        wait "$gateway_pid" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

# --- Frontend deps ----------------------------------------------------------
# `npm ci` is exact and reproducible but slow when nothing changed; only run
# if node_modules is missing. The user can `npm ci` manually after a pull.
if [[ ! -d frontend/node_modules ]]; then
    echo "==> Installing frontend dependencies (npm ci)"
    npm --prefix frontend ci
fi

# --- Gateway ----------------------------------------------------------------
# --foreground keeps the gateway attached to this script so Ctrl-C kills it.
# Cargo prints its progress to stderr; we leave both streams interleaved with
# vite's output so the user can see compile errors and request logs together.
echo "==> Starting gateway on port ${gateway_port} (cargo run, debug)"
(
    cargo run -p swage-gateway -- \
        gateway start --foreground --port "$gateway_port"
) &
gateway_pid=$!

# --- Frontend dev server ----------------------------------------------------
echo "==> Starting Vite dev server on ${vite_host}:${vite_port}"
(
    npm --prefix frontend run dev -- --host "$vite_host" --port "$vite_port"
) &
vite_pid=$!

# Best-effort LAN IP for printing the reachable URL.
lan_ip=""
if command -v hostname >/dev/null 2>&1; then
    lan_ip="$(hostname -I 2>/dev/null | awk '{print $1}')"
fi

echo
echo "    Gateway: http://127.0.0.1:${gateway_port}"
echo "    Web UI:  http://localhost:${vite_port}"
if [[ -n "$lan_ip" ]]; then
    echo "    LAN UI:  http://${lan_ip}:${vite_port}"
else
    echo "    LAN UI:  http://<this-machine-ip>:${vite_port}"
fi
echo
echo "    LAN exposure warning: the dev server has no authentication."
echo "    Only run this on networks you trust."
echo "    Ctrl-C to stop both."
echo

# Wait on whichever process exits first; the trap then takes the other down.
wait -n "$gateway_pid" "$vite_pid"
