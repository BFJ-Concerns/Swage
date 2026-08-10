# Swage

A context-governed agent platform for your own hardware: an always-on
gateway running agents that work on schedules and on demand, with everything
the system does recorded locally and reconstructable afterwards.

## Build and run

Requires the Rust toolchain (see `rust-toolchain.toml`) and Node for the web
UI. The gateway listens on loopback only.

```bash
cargo build --bin swage
./target/debug/swage gateway start --port 3000

# Web UI (development)
cd frontend && npm install && npm run dev
```

The gateway daemonises and logs to `~/.swage/gateway.log`. Stop it with
`swage gateway stop`.

## Configure a provider first

A fresh install runs, but no turn can complete until a model provider is
configured — the composer will say so and point at Settings. Configuration
lives in `~/.swage/config.toml`. A minimal working example:

```toml
[providers]
default = "anthropic"

[providers.entries.anthropic]
protocol = "anthropic"                # or "openai" for OpenAI-shaped endpoints
api_key_env = "ANTHROPIC_API_KEY"     # read from the environment at startup
# base_url = "https://..."            # only for proxies or self-hosted routes

[models]
default = "sonnet"

[models.entries.sonnet]
provider_id = "anthropic"
model_id = "claude-sonnet-5"
```

Set the key in the gateway's environment before starting it. Providers and
models can also be managed from the web UI under Settings → Providers.

`SWAGE_HOME` overrides the whole data root (config, database, logs) — useful
for throwaway instances: `SWAGE_HOME=/tmp/swage-test swage gateway start`.

## Where things live on disk

Everything is under `~/.swage/` (or `$SWAGE_HOME`):

- `config.toml` — providers, models, sandboxing, and platform settings.
- `instructions/` — global operator instructions, as `.md` files composed in
  filename order (`01-base.md`, `02-style.md`, …). They layer *after* the
  built-in orientation: every session already knows what it is running on,
  where it is, and what tools it has.
- `skills/<name>/` — global skills, one directory per skill with frontmatter.
- `spaces/<space>/` — each space's world: its own `instructions/`,
  `skills/`, `agents/<agent>/` (per-agent `instructions/` and `skills/`),
  memory, and knowledge-base bodies.
- `swage.db` — the SQLite store: conversations, turns, the audit trail,
  knowledge base, workflow runs. The one local record everything is
  reconstructable from.

The configuration cascade is Global → Space → Agent: the more specific
level extends or shadows the more general one.

## Development

`just verify` runs the full gate (format, clippy, tests, frontend checks,
projection freshness). See `CLAUDE.md` for repository conventions and
`docs/` for architecture diagrams.
