---
name: run-swage
description: Build, run, and drive Swage. Use when asked to start Swage, launch the gateway or web UI, screenshot the interface, drive the app to verify a change, run the TUI, or build/test the workspace.
---

Swage is a local AI-assistant platform. Its main surface is a Svelte SPA served
by the `swage` gateway binary — the layer almost every UI PR touches. Drive that
running SPA headlessly via `.claude/skills/run-swage/driver.mjs` (a Playwright
script that screenshots, probes, and clicks through the app). The terminal UI
(`swage tui`) is a secondary surface, covered below.

All paths below are relative to the repository root (the `swage` unit). The
driver lives at `.claude/skills/run-swage/driver.mjs`.

## Prerequisites

No `apt-get` packages needed installing in this container — the toolchain and
Playwright's Chromium were already present. A fresh machine needs:

- **Rust 1.95** (pinned by `rust-toolchain.toml`), `cargo` on PATH.
- **Node** (verified with v24) and **npm** on PATH.
- **`just`** (verified 1.51) — the task runner that wraps `xtask`.
- **`tmux`** — only for driving the TUI.
- **Playwright Chromium** for the web driver. npm installs the Playwright
  package; provision the matching browser revision explicitly:

```bash
npm --prefix desktop ci
npm --prefix desktop exec playwright install chromium
# On a bare machine also run, for the system libs Chromium needs:
#   npm --prefix desktop exec playwright install --with-deps chromium
```

The driver borrows this Chromium from `desktop/node_modules` — there is no
`chromium-cli` on this machine and no second browser download.

## Build

`just release` builds the frontend bundle and the release binary, embedding
`frontend/dist` into the binary via `rust-embed`:

```bash
just release        # → target/release/swage (frontend embedded)
```

Incremental builds take ~40s; a cold build is several minutes (large workspace,
17 crates). The binary serves the embedded SPA directly — no separate web server.

## Run (agent path)

Launch the gateway in the **foreground** against a fully isolated data root, wait
for the health endpoint, then drive it. Isolating both `HOME` and
`XDG_RUNTIME_DIR` keeps the run non-destructive and stops it colliding with any
real gateway you have running (see Gotchas).

```bash
# 1. Fresh, throwaway state. The .swage root MUST pre-exist or startup aborts.
export SWAGE_RUN=/tmp/swage-run
rm -rf "$SWAGE_RUN" && mkdir -p "$SWAGE_RUN/.swage" "$SWAGE_RUN/run"

# 2. Launch the gateway in the background (foreground mode, isolated env).
HOME="$SWAGE_RUN" XDG_RUNTIME_DIR="$SWAGE_RUN/run" \
  target/release/swage gateway start --foreground --host 127.0.0.1 --port 3940 \
  > "$SWAGE_RUN/gateway.out" 2>&1 &

# 3. Wait for readiness (no API key needed — the UI loads regardless).
timeout 30 bash -c 'until curl -sf http://127.0.0.1:3940/api/health >/dev/null; do sleep 0.3; done'

# 4. Drive the running SPA.
node .claude/skills/run-swage/driver.mjs screenshot http://127.0.0.1:3940 --out /tmp/swage-home.png
node .claude/skills/run-swage/driver.mjs flow      http://127.0.0.1:3940 --out /tmp/swage-flow.png

# 5. Stop it (same isolated env so it finds the right metadata).
HOME="$SWAGE_RUN" XDG_RUNTIME_DIR="$SWAGE_RUN/run" target/release/swage gateway stop
```

Stop with `swage gateway stop`, **not** `pkill -f 'swage gateway start'` — that
pattern matches the very shell running `pkill` (its command line contains the
string), killing your shell along with the gateway.

Screenshots land wherever `--out` points (default `/tmp/swage-screenshot.png`).

The driver's commands:

| command | what it does |
|---|---|
| `screenshot <url> [--route PATH] [--out FILE] [--wait SEL] [--full-page] [--settle MS]` | Navigate, wait for the SPA to mount, screenshot. `--route` deep-links into a SPA path. |
| `probe <url> [--route PATH]` | Print DOM landmarks (title, headings, button labels) and console errors as JSON — no screenshot. |
| `flow <url> [--out FILE]` | End-to-end: opens the bench picker → clicks the default bench → "Open", screenshots the workspace, reports landmarks. The template for scripted UI flows. |

The app opens on a **bench picker** ("Choose where to work"). `flow` clicks
through it to the assistant workspace; `screenshot` captures whatever route you
point at. Extend `flow` with `getByRole(...).click()` / `.fill(...)` steps to
reach deeper UI (settings, builder) as a PR demands.

## Run: TUI

The terminal UI connects to a running gateway. Launch it under tmux (it needs a
terminal) and point it at the isolated gateway from above:

```bash
tmux new-session -d -s swage-tui -x 200 -y 50
tmux send-keys -t swage-tui \
  'HOME=/tmp/swage-run XDG_RUNTIME_DIR=/tmp/swage-run/run target/release/swage tui --url http://127.0.0.1:3940' Enter
timeout 20 bash -c 'until tmux capture-pane -t swage-tui -p | grep -q "Default"; do sleep 0.3; done'
tmux capture-pane -t swage-tui -p          # ● Default + Conversation pane = connected
tmux kill-session -t swage-tui
```

## Run (human path)

`just dev` runs the debug binary plus the Vite dev server with hot reload; the
gateway redirects browser routes to Vite (via `SWAGE_DEV_FRONTEND_PORT`):

```bash
just dev        # gateway :3000 + Vite :5173, Ctrl-C stops both. Useless headless.
```

For a normal background daemon (writes its own `~/.swage` state):
`swage gateway start` then open `http://127.0.0.1:3000`. Stop with `swage gateway stop`.

## Test

```bash
cargo test --workspace                       # ~2,200 Rust tests
npm --prefix frontend test                   # Svelte/vitest suite
just test                                    # both of the above + desktop smoke
```

`just test` also runs the desktop Playwright smoke (`desktop/tests/smoke.mjs`),
which drives a **mock** gateway — not the real binary. The real-app harness is
this skill's `driver.mjs`.

## Gotchas

- **The SQLite DB ignores `SWAGE_HOME`.** It is hardcoded to
  `$HOME/.swage/swage.db` (gateway `startup.rs`). `SWAGE_HOME` only redirects
  *config* discovery. To isolate state you must override **`HOME`**, not
  `SWAGE_HOME` — otherwise the run mutates your real `~/.swage/swage.db` and you
  see your real conversations.
- **The `.swage` root must exist before launch.** Startup aborts with
  `Swage root directory '…/.swage' does not exist` if it is missing. `mkdir -p`
  it first (the DB-dir creation happens *after* config discovery, which is what
  fails).
- **Runtime metadata lives in `$XDG_RUNTIME_DIR/swage/metadata.json`** (falling
  back to `$HOME/.swage/run/`). `gateway start` reads it to detect an
  already-running gateway and *refuses to start* if one is alive. Isolate
  `XDG_RUNTIME_DIR` too, or your isolated launch may be blocked by — or collide
  with — your real background gateway.
- **No API key is required to load the UI.** Startup logs `No default
  provider/model pair is configured` and serves the UI fine; only *sending a
  message* fails until a provider is configured. Screenshotting needs no key.
- **The frontend is embedded at compile time.** Rebuilding `frontend/dist`
  after building the binary does nothing until the gateway crate recompiles.
  `just release` handles this (it re-embeds via content stamps); don't expect a
  bare `vite build` to change what the release binary serves.
- **No `chromium-cli`; Playwright is CommonJS.** The driver resolves Playwright
  from `desktop/node_modules` and reads `chromium` off the `.default` export
  (CJS/ESM interop). It launches Chromium with `--no-sandbox` for the container.

## Troubleshooting

- **`Swage root directory '…/.swage' does not exist — create it to get started`**:
  config discovery ran before the data dir was created. `mkdir -p "$HOME/.swage"`.
- **`Gateway is already running (PID …)`** on `gateway start`: a live metadata
  file was found. Either it's your real gateway (isolate `XDG_RUNTIME_DIR` as
  above) or a stale one — `swage gateway stop`, or remove
  `$XDG_RUNTIME_DIR/swage/metadata.json`.
- **`Frontend not built` (503 from the gateway)**: the binary has no embedded
  assets. Run `just release` (or `just build-dev-embedded` for a debug binary),
  not a plain `cargo build`.
- **Driver: `Cannot read properties of undefined (reading 'launch')`**:
  Playwright didn't resolve. Run `npm --prefix desktop ci`.
- **Driver: `Executable doesn't exist` under `~/.cache/ms-playwright`**:
  the Playwright package resolved, but its browser revision is not installed.
  Run `npm --prefix desktop exec playwright install chromium`.
- **Driver hangs / `waitForFunction` times out**: the SPA didn't mount — usually
  the bundle 404'd. Check `probe` output for console errors and confirm
  `curl http://127.0.0.1:PORT/` returns the SPA `index.html`, not a 503.
