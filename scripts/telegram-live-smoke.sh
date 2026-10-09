#!/usr/bin/env bash
set -euo pipefail

# Manual smoke only. Do not run in CI.
# Requires a real Telegram bot token in TELEGRAM_BOT_TOKEN.

if [[ -z "${TELEGRAM_BOT_TOKEN:-}" ]]; then
  echo "TELEGRAM_BOT_TOKEN must be set" >&2
  exit 1
fi

SWAGE_RUN="${SWAGE_RUN:-/tmp/swage-telegram-live-smoke}"
PORT="${SWAGE_TELEGRAM_SMOKE_PORT:-3942}"

rm -rf "$SWAGE_RUN"
mkdir -p "$SWAGE_RUN/home"

cat > "$SWAGE_RUN/home/config.toml" <<'TOML'
dangerously_skip_permissions = true

[channels.telegram]
bot_token_env = "TELEGRAM_BOT_TOKEN"
poll_timeout_secs = 5
TOML

echo "Starting isolated gateway on http://127.0.0.1:${PORT}"
SWAGE_HOME="$SWAGE_RUN/home" \
  cargo run -- gateway start --foreground --host 127.0.0.1 --port "$PORT" \
  > "$SWAGE_RUN/gateway.out" 2>&1 &
GATEWAY_PID=$!

cleanup() {
  SWAGE_HOME="$SWAGE_RUN/home" \
    cargo run -- gateway stop >/dev/null 2>&1 || true
  wait "$GATEWAY_PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

timeout 60 bash -c "until curl -sf http://127.0.0.1:${PORT}/api/health >/dev/null; do sleep 0.5; done"

response="$(
  curl -sf \
    -H 'content-type: application/json' \
    -d '{"integration":"telegram"}' \
    "http://127.0.0.1:${PORT}/api/onboarding/channels/link-codes"
)"
link_code="$(printf '%s' "$response" | sed -n 's/.*"link_code":"\([^"]*\)".*/\1/p')"

if [[ -z "$link_code" ]]; then
  echo "Could not parse link code from response: $response" >&2
  exit 1
fi

echo
echo "Send this to the Telegram bot from the chat to link:"
echo "  /link ${link_code}"
echo
echo "Gateway log: $SWAGE_RUN/gateway.out"
echo "Press Ctrl-C when finished."

tail -f "$SWAGE_RUN/gateway.out"
