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
    stop_process_tree "$vite_pid"
    stop_process_tree "$gateway_pid"
}
trap cleanup EXIT INT TERM

stop_process_tree() {
    local pid="${1:-}"
    [[ -n "$pid" ]] || return 0
    kill -0 "$pid" 2>/dev/null || return 0

    local child
    while read -r child; do
        [[ -n "$child" ]] || continue
        stop_process_tree "$child"
    done < <(pgrep -P "$pid" 2>/dev/null || true)

    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
}

# --- Build cache hygiene ----------------------------------------------------
# Cap target/ at ~50 GiB by evicting least-recently-used artefacts. Time-based
# sweeps don't help an actively-built workspace (every dep gets touched on every
# build), so we cap by size instead. `--maxsize` takes megabytes.
# Best-effort: skipped silently if cargo-sweep isn't installed (run
# `cargo install cargo-sweep` to enable). Never fails the dev loop.
if command -v cargo-sweep >/dev/null 2>&1; then
    echo "==> Capping target/ at 50 GiB (cargo sweep --maxsize)"
    cargo sweep --maxsize 50000 || true
else
    echo "==> Skipping cargo sweep (install with: cargo install cargo-sweep)"
fi

# --- Frontend deps ----------------------------------------------------------
# `npm ci` is exact and reproducible but slow when nothing changed; only run
# if node_modules is missing. The user can `npm ci` manually after a pull.
if [[ ! -d frontend/node_modules ]]; then
    echo "==> Installing frontend dependencies (npm ci)"
    npm --prefix frontend ci
fi

# --- Gateway ----------------------------------------------------------------
# Build before starting either long-running process so cargo's compile output
# does not bury the live Vite URL and make the gateway port look authoritative.
echo "==> Building gateway (cargo build, debug)"
cargo build -p swage-gateway

# --foreground keeps the gateway attached to this script so Ctrl-C kills it.
echo "==> Starting gateway API on port ${gateway_port} (debug binary)"
(
    SWAGE_DEV_FRONTEND_PORT="$vite_port" target/debug/swage \
        gateway start --foreground --port "$gateway_port"
) &
gateway_pid=$!

# --- Frontend dev server ----------------------------------------------------
echo "==> Starting Vite dev server on ${vite_host}:${vite_port}"
(
    npm --prefix frontend run dev -- --host "$vite_host" --port "$vite_port" --strictPort
) &
vite_pid=$!

# Best-effort LAN IP for printing the reachable URL.
lan_ip=""
if command -v hostname >/dev/null 2>&1; then
    # Some hostname implementations do not support -I. Keep this lookup
    # genuinely best-effort so a missing LAN hint does not kill the dev loop.
    lan_ip="$({ hostname -I 2>/dev/null || true; } | awk 'NF { print $1; exit }')"
fi
if [[ -z "$lan_ip" ]] && command -v ip >/dev/null 2>&1; then
    lan_ip="$({ ip -o -4 route get 1.1.1.1 2>/dev/null || true; } | awk '{ for (i = 1; i <= NF; i++) if ($i == "src") { print $(i + 1); exit } }')"
fi

echo
echo "    Gateway API:       http://127.0.0.1:${gateway_port}"
echo "    Web UI:            http://localhost:${vite_port}"
echo "    Gateway UI route:  redirects to the live Vite UI"
if [[ -n "$lan_ip" ]]; then
    echo "    LAN UI:            http://${lan_ip}:${vite_port}"
else
    echo "    LAN UI:            http://<this-machine-ip>:${vite_port}"
fi
echo
echo "    LAN exposure warning: the dev server has no authentication."
echo "    Only run this on networks you trust."
echo "    Ctrl-C to stop both."
echo

# Wait on whichever process exits first; the trap then takes the other down.
wait -n "$gateway_pid" "$vite_pid"
